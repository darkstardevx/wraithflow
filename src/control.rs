//! A tiny read-only control socket for external tools (a future TUI, a
//! monitoring script) to query live pipeline stats without scraping
//! journald. Opt-in via `control_socket` in config.toml — disabled
//! unless set, so no deployment gets new attack surface it didn't ask
//! for.
//!
//! One command in, one JSON line out, connection closes — a client
//! polls by reconnecting on its own refresh interval rather than this
//! server managing per-client streaming state.

use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use wf_core::{StatsRegistry, StatsSnapshot};

pub async fn serve(socket_path: &Path, stats: StatsRegistry) -> std::io::Result<()> {
    // A stale socket file from an unclean shutdown (kill -9, a crash)
    // would otherwise make bind() fail with "address in use".
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    // Owner-only: these stats aren't deeply sensitive, but pipeline
    // names and traffic volume have no reason to be readable by every
    // local user by default.
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(
        "\x1b[35m[control]\x1b[0m Listening on {}",
        socket_path.display()
    );

    loop {
        let (conn, _) = listener.accept().await?;
        let stats = stats.clone();
        tokio::spawn(async move {
            let _ = handle_connection(conn, &stats).await;
        });
    }
}

async fn handle_connection(conn: UnixStream, stats: &StatsRegistry) -> std::io::Result<()> {
    let (reader, mut writer) = conn.into_split();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).await?;

    let response = match parse_command(line.trim()) {
        Ok(Command::Stats) => stats_response(&stats.snapshot_all()),
        Err(msg) => json!({ "error": msg }).to_string(),
    };

    writer.write_all(response.as_bytes()).await?;
    writer.write_all(b"\n").await
}

/// Every supported request. One variant today, shaped so a second
/// command is a small diff later rather than a rewrite -- without
/// building handling for a command nothing needs yet.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Stats,
}

fn parse_command(line: &str) -> Result<Command, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
    match value.get("cmd").and_then(|c| c.as_str()) {
        Some("stats") => Ok(Command::Stats),
        Some(other) => Err(format!("unknown command: {other}")),
        None => Err("missing \"cmd\" field".to_string()),
    }
}

fn stats_response(snapshots: &[(String, StatsSnapshot)]) -> String {
    let pipelines: Vec<_> = snapshots
        .iter()
        .map(|(name, s)| {
            json!({
                "name": name,
                "bytes_in": s.bytes_in,
                "bytes_out": s.bytes_out,
                "connections_total": s.connections_total,
                "connections_active": s.connections_active,
                "errors_total": s.errors_total,
            })
        })
        .collect();
    json!({ "pipelines": pipelines }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn parse_command_recognizes_stats() {
        assert_eq!(parse_command(r#"{"cmd":"stats"}"#), Ok(Command::Stats));
    }

    #[test]
    fn parse_command_rejects_an_unknown_command() {
        assert!(parse_command(r#"{"cmd":"reload"}"#).is_err());
    }

    #[test]
    fn parse_command_rejects_missing_cmd_field() {
        assert!(parse_command(r#"{}"#).is_err());
    }

    #[test]
    fn parse_command_rejects_malformed_json() {
        assert!(parse_command("not json").is_err());
    }

    #[test]
    fn stats_response_includes_every_field_for_every_pipeline() {
        let registry = StatsRegistry::new();
        let s = registry.get_or_create("http-traffic-gateway");
        s.bytes_in
            .fetch_add(10, std::sync::atomic::Ordering::Relaxed);
        s.bytes_out
            .fetch_add(20, std::sync::atomic::Ordering::Relaxed);

        let json = stats_response(&registry.snapshot_all());
        assert!(json.contains("\"name\":\"http-traffic-gateway\""));
        assert!(json.contains("\"bytes_in\":10"));
        assert!(json.contains("\"bytes_out\":20"));
        assert!(json.contains("\"connections_total\":0"));
        assert!(json.contains("\"connections_active\":0"));
        assert!(json.contains("\"errors_total\":0"));
    }

    #[test]
    fn stats_response_with_no_pipelines_is_an_empty_list() {
        assert_eq!(stats_response(&[]), r#"{"pipelines":[]}"#);
    }

    #[tokio::test]
    async fn serve_answers_a_real_stats_request_over_a_real_socket() {
        let socket_path =
            std::env::temp_dir().join(format!("wf-control-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);

        let registry = StatsRegistry::new();
        registry
            .get_or_create("test-pipeline")
            .connections_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let server_path = socket_path.clone();
        let server_registry = registry.clone();
        let server = tokio::spawn(async move { serve(&server_path, server_registry).await });

        // Give the listener a moment to bind before connecting.
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut client = UnixStream::connect(&socket_path).await.unwrap();
        client.write_all(b"{\"cmd\":\"stats\"}\n").await.unwrap();
        client.shutdown().await.unwrap();

        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();

        assert!(response.contains("\"name\":\"test-pipeline\""));
        assert!(response.contains("\"connections_total\":1"));

        server.abort();
        let _ = std::fs::remove_file(&socket_path);
    }
}
