//! A minimal live dashboard over WraithFlow's read-only control socket
//! (`../../src/control.rs`) — polls `{"cmd":"stats"}` on an interval
//! and renders a table of pipeline health. A separate binary, not a
//! mode of the daemon itself, so the systemd-deployed `wraithflow`
//! binary never carries `ratatui`/`crossterm`'s dependency tree.
//!
//! Fully synchronous by design: the control socket protocol is a
//! one-shot connect/write/read/close per poll, and `crossterm`'s event
//! loop is blocking anyway — mixing in an async runtime for one
//! blocking round-trip a second would add complexity (and a
//! dependency) for no benefit.

use clap::Parser;
use crossterm::event::{self, Event, KeyCode};
use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "wf-tui",
    about = "Live dashboard for WraithFlow's control socket"
)]
struct Args {
    /// Path to WraithFlow's control socket (see `control_socket` in
    /// config.toml). Defaults to the path used in the example config.
    #[arg(long, default_value = "/run/wraithflow/control.sock")]
    socket: PathBuf,

    /// How often to reconnect and refresh, in seconds.
    #[arg(long, default_value_t = 1)]
    interval: u64,
}

#[derive(Deserialize, Debug, Clone)]
struct PipelineStat {
    name: String,
    bytes_in: u64,
    bytes_out: u64,
    connections_total: u64,
    connections_active: u64,
    errors_total: u64,
}

/// Mirrors the control socket's wire shape exactly (`src/control.rs`'s
/// `stats_response`) — deliberately not a shared type with `wf-core`,
/// same decoupling the control socket itself already uses: the JSON
/// contract is the interface, not an internal Rust type.
#[derive(Deserialize, Debug, Default)]
struct StatsResponse {
    #[serde(default)]
    pipelines: Vec<PipelineStat>,
}

fn fetch_stats(socket_path: &Path) -> std::io::Result<StatsResponse> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.write_all(b"{\"cmd\":\"stats\"}\n")?;
    stream.shutdown(Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    serde_json::from_str(response.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `cybercore::palette::hex()` already returns exactly this for
/// "anything that wants the color value itself rather than a terminal
/// escape" (its own doc comment) -- this is the missing half, turning
/// that hex string into something Ratatui can render with. No
/// existing hex->Ratatui-Color helper exists anywhere in the
/// workspace yet (`cyberfleet`/`cyberplug` don't depend on
/// `cybercore` at all).
fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Looks up a named CYBERGRID color and converts it, falling back to a
/// plain ANSI color if the palette lookup or hex parse ever fails --
/// this is decoration, not something that should ever crash the TUI.
fn cybercore_color(name: &str, fallback: Color) -> Color {
    cybercore::palette::hex(name)
        .and_then(|h| parse_hex_color(&h))
        .unwrap_or(fallback)
}

/// Errors highlight red regardless of activity (something needs
/// attention even if the pipeline looks otherwise idle); an idle,
/// error-free pipeline is muted rather than "healthy" green, so green
/// reads as "actually carrying traffic right now."
fn row_style(pipeline: &PipelineStat, healthy: Color, error: Color, muted: Color) -> Style {
    if pipeline.errors_total > 0 {
        Style::new().fg(error)
    } else if pipeline.connections_active > 0 {
        Style::new().fg(healthy)
    } else {
        Style::new().fg(muted)
    }
}

fn build_rows(
    pipelines: &[PipelineStat],
    healthy: Color,
    error: Color,
    muted: Color,
) -> Vec<Row<'static>> {
    pipelines
        .iter()
        .map(|p| {
            Row::new(vec![
                p.name.clone(),
                format!("{}/{}", p.connections_active, p.connections_total),
                format!("{}B", p.bytes_in),
                format!("{}B", p.bytes_out),
                p.errors_total.to_string(),
            ])
            .style(row_style(p, healthy, error, muted))
        })
        .collect()
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let healthy = cybercore_color("acid_green", Color::Green);
    let error = cybercore_color("red", Color::Red);
    let muted = cybercore_color("muted", Color::DarkGray);

    let mut terminal = ratatui::init();
    let mut pipelines: Vec<PipelineStat> = Vec::new();
    let mut last_error: Option<String>;

    loop {
        match fetch_stats(&args.socket) {
            Ok(resp) => {
                pipelines = resp.pipelines;
                last_error = None;
            }
            Err(e) => last_error = Some(e.to_string()),
        }

        terminal.draw(|frame| {
            let header = Row::new(vec!["Pipeline", "Active/Total", "In", "Out", "Errors"])
                .style(Style::new().add_modifier(Modifier::BOLD));
            let rows = build_rows(&pipelines, healthy, error, muted);
            let widths = [
                Constraint::Fill(1),
                Constraint::Length(14),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(8),
            ];
            let title = match &last_error {
                Some(e) => format!("WraithFlow — disconnected ({e}) — 'q' to quit"),
                None => "WraithFlow — live pipeline stats — 'q' to quit".to_string(),
            };
            let table = Table::new(rows, widths)
                .header(header)
                .block(Block::default().borders(Borders::ALL).title(title));
            frame.render_widget(table, frame.area());
        })?;

        if event::poll(Duration::from_secs(args.interval))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_color_accepts_a_leading_hash_or_not() {
        assert_eq!(parse_hex_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_hex_color("ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_hex_color("00ff88"), Some(Color::Rgb(0, 255, 136)));
    }

    #[test]
    fn parse_hex_color_rejects_the_wrong_length() {
        assert_eq!(parse_hex_color("fff"), None);
        assert_eq!(parse_hex_color("ff00"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn parse_hex_color_rejects_non_hex_characters() {
        assert_eq!(parse_hex_color("zzzzzz"), None);
    }

    fn stat(name: &str, active: u64, errors: u64) -> PipelineStat {
        PipelineStat {
            name: name.to_string(),
            bytes_in: 0,
            bytes_out: 0,
            connections_total: active,
            connections_active: active,
            errors_total: errors,
        }
    }

    #[test]
    fn row_style_flags_errors_red_even_when_idle() {
        let healthy = Color::Green;
        let error = Color::Red;
        let muted = Color::DarkGray;
        let s = row_style(&stat("p", 0, 1), healthy, error, muted);
        assert_eq!(s.fg, Some(error));
    }

    #[test]
    fn row_style_marks_active_pipelines_healthy() {
        let healthy = Color::Green;
        let error = Color::Red;
        let muted = Color::DarkGray;
        let s = row_style(&stat("p", 3, 0), healthy, error, muted);
        assert_eq!(s.fg, Some(healthy));
    }

    #[test]
    fn row_style_marks_idle_error_free_pipelines_muted() {
        let healthy = Color::Green;
        let error = Color::Red;
        let muted = Color::DarkGray;
        let s = row_style(&stat("p", 0, 0), healthy, error, muted);
        assert_eq!(s.fg, Some(muted));
    }

    #[test]
    fn stats_response_deserializes_the_real_wire_shape() {
        let json = r#"{"pipelines":[{"name":"a","bytes_in":10,"bytes_out":20,"connections_total":1,"connections_active":1,"errors_total":0}]}"#;
        let resp: StatsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.pipelines.len(), 1);
        assert_eq!(resp.pipelines[0].name, "a");
        assert_eq!(resp.pipelines[0].bytes_in, 10);
    }

    #[test]
    fn stats_response_defaults_to_an_empty_list() {
        let resp: StatsResponse = serde_json::from_str("{}").unwrap();
        assert!(resp.pipelines.is_empty());
    }

    #[test]
    fn build_rows_produces_one_row_per_pipeline() {
        let pipelines = vec![stat("a", 1, 0), stat("b", 0, 2)];
        let rows = build_rows(&pipelines, Color::Green, Color::Red, Color::DarkGray);
        assert_eq!(rows.len(), 2);
    }
}
