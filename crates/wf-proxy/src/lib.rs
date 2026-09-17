use std::fs::{File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use wf_core::{BufferPool, Direction, PipelineStats};
use wf_packet::{OutputFormat, PacketFilter, Redactor};

/// An optional, shared JSONL sink for every logged packet across every
/// pipeline (not per-pipeline — one stream, like `println!`'s stdout
/// already is). Exists so a separate tool (Echo) can browse captured
/// traffic instead of only ever seeing it scroll past in
/// `journalctl -u wraithflow`. Deliberately doesn't replace or change the
/// existing `println!` — this is an addition, not a redirect.
pub struct CaptureLog {
    file: Mutex<File>,
}

impl CaptureLog {
    /// Opens (creating parent directories and the file itself if needed)
    /// for appending. One open per process lifetime — the caller holds
    /// this in an `Arc`, cloned into every pipeline's `OutputSpec`.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Appends one line. Errors are logged, not propagated — a capture-log
    /// write failure (disk full, permissions) shouldn't take down the
    /// proxy itself, the same reasoning as the existing packet-recycling
    /// and stats-update paths that also never fail the connection.
    fn write_line(&self, line: &str) {
        let mut file = match self.file.lock() {
            Ok(f) => f,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(e) = writeln!(file, "{line}") {
            tracing::warn!("[Capture Log] failed to write: {e}");
        }
    }
}

/// One logged packet, exactly as written to the capture log — `rendered`
/// is the same string `println!` already printed (same formatting,
/// same redaction already applied), not a second independent rendering.
#[derive(serde::Serialize)]
struct CaptureRecord<'a> {
    pipeline: &'a str,
    direction: &'static str,
    at: String,
    format: &'static str,
    rendered: &'a str,
}

/// Everything that controls how a pipeline logs the traffic crossing it.
/// `enabled: false` is the fast path — no `Packet` gets built at all, so a
/// muted pipeline (e.g. a DB relay) pays zero formatting cost per byte.
#[derive(Clone)]
pub struct OutputSpec {
    pub enabled: bool,
    pub format: OutputFormat,
    pub pretty: bool,
    pub color: bool,
    pub filter: PacketFilter,
    /// Applied to bytes that pass `filter`, before rendering — masks
    /// matching substrings with `*` so they never reach the log.
    pub redact: Redactor,
    /// Colors matching substrings (Raw/Compact formats only). Applied
    /// after `redact`, so a highlight pattern can't un-hide a redacted one.
    pub highlight: Vec<Vec<u8>>,
    /// Shared across every pipeline — `None` unless `capture_log` is set
    /// in the top-level config, in which case every pipeline writes into
    /// the same file (see `CaptureLog`'s own doc comment for why).
    pub capture_log: Option<Arc<CaptureLog>>,
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            format: OutputFormat::Hexdump,
            pretty: false,
            color: true,
            filter: PacketFilter::default(),
            redact: Redactor::default(),
            highlight: Vec::new(),
            capture_log: None,
        }
    }
}

/// Everything a pipeline needs to participate in a graceful shutdown:
/// `token` is cancelled to signal "stop accepting new connections",
/// `tracker` tracks in-flight sessions (spawned via `tracker.spawn`
/// rather than a bare `tokio::spawn`) so the caller can bound how long
/// it waits for them via `tracker.close()` + `tracker.wait()`. Bundled
/// together since every call site needs both or neither.
#[derive(Clone)]
pub struct Shutdown {
    pub token: CancellationToken,
    pub tracker: TaskTracker,
}

/// Accepts connections until `shutdown.token` is cancelled, at which
/// point it stops accepting *new* connections and returns --
/// already-accepted sessions keep running untouched. This function has
/// no opinion on a drain deadline; that's the caller's call via
/// `shutdown.tracker`.
pub async fn start_proxy(
    name: &str,
    listen_addr: &str,
    target_addr: &str,
    output: OutputSpec,
    stats: Arc<PipelineStats>,
    buffer_pool: Arc<BufferPool>,
    shutdown: Shutdown,
) -> io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    tracing::info!(
        "\x1b[35m[{}]\x1b[0m Pipeline active on {} -> forwarding to {}",
        name,
        listen_addr,
        target_addr
    );

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (client_stream, client_addr) = accepted?;
                let target_string = target_addr.to_string();
                let name = name.to_string();
                let output = output.clone();
                let stats = stats.clone();
                let buffer_pool = buffer_pool.clone();

                shutdown.tracker.spawn(async move {
                    tracing::debug!(
                        "\x1b[36m[+ Flow Connected]\x1b[0m Connection tracked from {}",
                        client_addr
                    );
                    stats.connections_total.fetch_add(1, Ordering::Relaxed);
                    stats.connections_active.fetch_add(1, Ordering::Relaxed);

                    if let Err(e) = handle_session(
                        client_stream,
                        &target_string,
                        &name,
                        &output,
                        &stats,
                        &buffer_pool,
                    )
                    .await
                    {
                        stats.errors_total.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("\x1b[31m[! Flow Error]\x1b[0m Pipeline ruptured: {}", e);
                    }

                    stats.connections_active.fetch_sub(1, Ordering::Relaxed);
                    tracing::debug!(
                        "\x1b[33m[- Flow Disconnected]\x1b[0m Session closed for {}",
                        client_addr
                    );
                });
            }
            () = shutdown.token.cancelled() => {
                tracing::info!(
                    "\x1b[35m[{}]\x1b[0m Draining -- no longer accepting new connections",
                    name
                );
                return Ok(());
            }
        }
    }
}

async fn handle_session(
    mut client_stream: TcpStream,
    target_addr: &str,
    pipeline_name: &str,
    output: &OutputSpec,
    stats: &Arc<PipelineStats>,
    buffer_pool: &Arc<BufferPool>,
) -> io::Result<()> {
    let mut target_stream = TcpStream::connect(target_addr).await?;

    // Break streams down into readable/writable raw splits
    let (mut client_reader, mut client_writer) = client_stream.split();
    let (mut target_reader, mut target_writer) = target_stream.split();

    // Outbound Interception Loop (Client -> Target)
    let client_to_target = async {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = client_reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            } // EOF reached

            stats
                .bytes_out
                .fetch_add(bytes_read as u64, Ordering::Relaxed);
            log_chunk(
                pipeline_name,
                Direction::Outbound,
                &buffer[..bytes_read],
                output,
                buffer_pool,
            );
            target_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Inbound Interception Loop (Target -> Client)
    let target_to_client = async {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = target_reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            } // EOF reached

            stats
                .bytes_in
                .fetch_add(bytes_read as u64, Ordering::Relaxed);
            log_chunk(
                pipeline_name,
                Direction::Inbound,
                &buffer[..bytes_read],
                output,
                buffer_pool,
            );
            client_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Whichever direction finishes first (EOF or error) ends the whole
    // session -- `try_join!` (wait for both) was tried first and found
    // to have a real hang: if the client fully disconnects and the
    // target never independently sends more data or closes on its own,
    // the still-blocked other-direction read means the session (and its
    // TaskTracker entry) never finishes, which defeats a bounded drain
    // on shutdown. Both `TcpStream`s are dropped when this function
    // returns, closing both sides regardless of which direction won.
    tokio::select! {
        result = client_to_target => result,
        result = target_to_client => result,
    }
}

fn log_chunk(
    pipeline_name: &str,
    direction: Direction,
    bytes: &[u8],
    output: &OutputSpec,
    pool: &BufferPool,
) {
    if !output.enabled {
        return;
    }
    let mut packet = wf_core::Packet::pooled(pool, pipeline_name, direction, bytes);
    if !output.filter.matches(&packet) {
        packet.recycle(pool);
        return;
    }
    if !output.redact.is_empty() {
        output.redact.apply(&mut packet.bytes);
    }
    let rendered = wf_packet::render(
        &packet,
        output.format,
        output.pretty,
        output.color,
        &output.highlight,
    );
    println!("{rendered}");
    if let Some(capture_log) = &output.capture_log {
        // Rendered fresh with color:false — `rendered` above may carry
        // embedded ANSI escape codes (whatever `output.color` says for
        // this pipeline's *terminal* output), but a consumer like Echo
        // applies its own cybercore theme in its own UI and shouldn't
        // have to strip WraithFlow's ANSI codes first. Only pays this
        // extra render cost when a capture log is actually configured.
        let capture_rendered = if output.color {
            wf_packet::render(
                &packet,
                output.format,
                output.pretty,
                false,
                &output.highlight,
            )
        } else {
            rendered.clone()
        };
        let record = CaptureRecord {
            pipeline: pipeline_name,
            direction: packet.direction.as_str(),
            at: packet.timestamp.to_rfc3339(),
            format: output.format.as_str(),
            rendered: &capture_rendered,
        };
        match serde_json::to_string(&record) {
            Ok(line) => capture_log.write_line(&line),
            Err(e) => tracing::warn!("[Capture Log] failed to serialize record: {e}"),
        }
    }
    packet.recycle(pool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioTcpListener;
    use wf_core::PipelineStats;

    /// Binds a fake "target" that accepts one connection, echoes anything
    /// it receives, and keeps the connection open until the client closes
    /// it -- enough to prove a session survives a cancellation that's
    /// still supposed to let in-flight work finish.
    async fn spawn_echo_target() -> String {
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            loop {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                let _ = stream.write_all(&buf[..n]).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn cancelling_stops_new_connections_but_lets_existing_ones_finish() {
        let target_addr = spawn_echo_target().await;
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen_addr = listener.local_addr().unwrap().to_string();
        drop(listener); // free the port for start_proxy's own bind

        let shutdown = Shutdown {
            token: CancellationToken::new(),
            tracker: TaskTracker::new(),
        };
        let stats = Arc::new(PipelineStats::default());
        let pool = Arc::new(BufferPool::default());

        let proxy_shutdown = shutdown.clone();
        let proxy_listen = listen_addr.clone();
        let proxy_handle = tokio::spawn(async move {
            start_proxy(
                "test",
                &proxy_listen,
                &target_addr,
                OutputSpec {
                    enabled: false,
                    ..OutputSpec::default()
                },
                stats,
                pool,
                proxy_shutdown,
            )
            .await
        });

        // Give the listener a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A real client connects and exchanges data before shutdown.
        let mut client = TcpStream::connect(&listen_addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        // Cancel: the accept loop should stop taking new connections...
        shutdown.token.cancel();
        proxy_handle.await.unwrap().unwrap();
        assert!(TcpStream::connect(&listen_addr).await.is_err());

        // ...but the already-accepted session is still alive and working.
        client.write_all(b"world").await.unwrap();
        let mut buf2 = [0u8; 5];
        client.read_exact(&mut buf2).await.unwrap();
        assert_eq!(&buf2, b"world");

        // Only once the client actually disconnects does the tracked
        // session finish, and only then does wait() resolve.
        drop(client);
        shutdown.tracker.close();
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown.tracker.wait())
            .await
            .expect("session should drain quickly after the client disconnects");
    }

    #[test]
    fn log_chunk_writes_a_real_line_to_a_configured_capture_log() {
        let dir =
            std::env::temp_dir().join(format!("wf-proxy-capture-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("captures.jsonl");

        let capture_log = Arc::new(CaptureLog::open(&log_path).unwrap());
        let output = OutputSpec {
            format: OutputFormat::Compact,
            capture_log: Some(capture_log),
            ..OutputSpec::default()
        };
        let pool = BufferPool::default();

        log_chunk(
            "test-pipeline",
            Direction::Outbound,
            b"hello capture log",
            &output,
            &pool,
        );

        let contents = std::fs::read_to_string(&log_path).unwrap();
        let line = contents.lines().next().expect("expected exactly one line");
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(record["pipeline"], "test-pipeline");
        assert_eq!(record["direction"], "OUTBOUND");
        assert_eq!(record["format"], "compact");
        assert!(record["rendered"].as_str().unwrap().contains("hello"));
        // Compact format includes ANSI color codes when `color: true` is
        // the pipeline's terminal setting (OutputSpec::default() sets it),
        // but the capture log always renders with color:false -- the
        // whole point of the separate render in log_chunk.
        assert!(!record["rendered"].as_str().unwrap().contains('\x1b'));

        std::fs::remove_dir_all(&dir).ok();
    }
}
