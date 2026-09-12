use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use wf_core::{BufferPool, Direction, PipelineStats};
use wf_packet::{OutputFormat, PacketFilter, Redactor};

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
        }
    }
}

pub async fn start_proxy(
    name: &str,
    listen_addr: &str,
    target_addr: &str,
    output: OutputSpec,
    stats: Arc<PipelineStats>,
    buffer_pool: Arc<BufferPool>,
) -> io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    println!(
        "\x1b[35m[{}]\x1b[0m Pipeline active on {} -> forwarding to {}",
        name, listen_addr, target_addr
    );

    loop {
        let (client_stream, client_addr) = listener.accept().await?;
        let target_string = target_addr.to_string();
        let name = name.to_string();
        let output = output.clone();
        let stats = stats.clone();
        let buffer_pool = buffer_pool.clone();

        tokio::spawn(async move {
            println!("\x1b[36m[+ Flow Connected]\x1b[0m Connection tracked from {}", client_addr);
            stats.connections_total.fetch_add(1, Ordering::Relaxed);
            stats.connections_active.fetch_add(1, Ordering::Relaxed);

            if let Err(e) = handle_session(client_stream, &target_string, &name, &output, &stats, &buffer_pool).await {
                stats.errors_total.fetch_add(1, Ordering::Relaxed);
                eprintln!("\x1b[31m[! Flow Error]\x1b[0m Pipeline ruptured: {}", e);
            }

            stats.connections_active.fetch_sub(1, Ordering::Relaxed);
            println!("\x1b[33m[- Flow Disconnected]\x1b[0m Session closed for {}", client_addr);
        });
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
            if bytes_read == 0 { break; } // EOF reached

            stats.bytes_out.fetch_add(bytes_read as u64, Ordering::Relaxed);
            log_chunk(pipeline_name, Direction::Outbound, &buffer[..bytes_read], output, buffer_pool);
            target_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Inbound Interception Loop (Target -> Client)
    let target_to_client = async {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = target_reader.read(&mut buffer).await?;
            if bytes_read == 0 { break; } // EOF reached

            stats.bytes_in.fetch_add(bytes_read as u64, Ordering::Relaxed);
            log_chunk(pipeline_name, Direction::Inbound, &buffer[..bytes_read], output, buffer_pool);
            client_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Keep pipelines executing simultaneously until closure
    tokio::try_join!(client_to_target, target_to_client)?;
    Ok(())
}

fn log_chunk(pipeline_name: &str, direction: Direction, bytes: &[u8], output: &OutputSpec, pool: &BufferPool) {
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
    println!("{}", wf_packet::render(&packet, output.format, output.pretty, output.color, &output.highlight));
    packet.recycle(pool);
}
