//! A live dashboard over WraithFlow's read-only control socket
//! (`../../src/control.rs`) — polls `{"cmd":"stats"}` on an interval
//! and renders a table of pipeline health, with sorting, pause,
//! per-pipeline history sparklines, and help/detail popups. A separate
//! binary, not a mode of the daemon itself, so the systemd-deployed
//! `wraithflow` binary never carries `ratatui`/`crossterm`'s
//! dependency tree.
//!
//! Fully synchronous by design: the control socket protocol is a
//! one-shot connect/write/read/close per poll, and `crossterm`'s event
//! loop is blocking anyway — mixing in an async runtime for one
//! blocking round-trip a second would add complexity (and a
//! dependency) for no benefit.

mod app;
mod ui;

use app::{handle_key, next_sort_field, Action, App, Mode, StatsResponse};
use clap::Parser;
use crossterm::event::{self, Event};
use ratatui::style::Color;
use ratatui::widgets::TableState;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use ui::Palette;

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

    /// Fetch one snapshot, print it as a plain table, and exit --
    /// no terminal setup at all, so it also works with no real
    /// controlling TTY. Script/cron-friendly.
    #[arg(long)]
    once: bool,

    /// Disable cybercore-derived coloring, in both --once output and
    /// the interactive dashboard.
    #[arg(long)]
    no_color: bool,
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

fn resolve_palette(no_color: bool) -> Palette {
    if no_color {
        return Palette {
            healthy: Color::Reset,
            error: Color::Reset,
            muted: Color::Reset,
            accent: Color::Reset,
            border: Color::Reset,
            text: Color::Reset,
        };
    }
    Palette {
        healthy: cybercore_color("acid_green", Color::Green),
        error: cybercore_color("red", Color::Red),
        muted: cybercore_color("muted", Color::DarkGray),
        accent: cybercore_color("purple", Color::Magenta),
        border: cybercore_color("line", Color::Gray),
        text: cybercore_color("white", Color::White),
    }
}

fn run_once(socket: &Path, no_color: bool) -> std::io::Result<()> {
    let resp = fetch_stats(socket)?;
    let (healthy, error, muted, reset) = if no_color {
        ("", "", "", "")
    } else {
        ("\x1b[92m", "\x1b[91m", "\x1b[90m", "\x1b[0m")
    };
    println!(
        "{:<24} {:>12} {:>12} {:>12} {:>8}",
        "PIPELINE", "ACTIVE/TOTAL", "IN", "OUT", "ERRORS"
    );
    for p in &resp.pipelines {
        let color = if p.errors_total > 0 {
            error
        } else if p.connections_active > 0 {
            healthy
        } else {
            muted
        };
        println!(
            "{color}{:<24} {:>12} {:>12} {:>12} {:>8}{reset}",
            p.name,
            format!("{}/{}", p.connections_active, p.connections_total),
            format!("{}B", p.bytes_in),
            format!("{}B", p.bytes_out),
            p.errors_total
        );
    }
    Ok(())
}

fn run_interactive(socket: &Path, interval: u64, no_color: bool) -> std::io::Result<()> {
    let palette = resolve_palette(no_color);
    let mut terminal = ratatui::init();
    let mut app = App::new();
    let mut table_state = TableState::default();

    loop {
        if !app.paused {
            match fetch_stats(socket) {
                Ok(resp) => {
                    app.update(resp.pipelines);
                    app.last_error = None;
                }
                Err(e) => app.last_error = Some(e.to_string()),
            }
        }

        terminal.draw(|frame| ui::draw(frame, &app, &mut table_state, &palette))?;

        if event::poll(Duration::from_secs(interval))? {
            if let Event::Key(key) = event::read()? {
                match handle_key(app.mode, key.code) {
                    Action::Quit => break,
                    Action::MoveUp => app.selected = app.selected.saturating_sub(1),
                    Action::MoveDown => {
                        if !app.pipelines.is_empty() {
                            app.selected = (app.selected + 1).min(app.pipelines.len() - 1);
                        }
                    }
                    Action::CycleSort => {
                        app.sort_field = next_sort_field(app.sort_field);
                        app::sort_pipelines(&mut app.pipelines, app.sort_field, app.sort_desc);
                    }
                    Action::ReverseSort => {
                        app.sort_desc = !app.sort_desc;
                        app::sort_pipelines(&mut app.pipelines, app.sort_field, app.sort_desc);
                    }
                    Action::TogglePause => app.paused = !app.paused,
                    Action::OpenHelp => app.mode = Mode::Help,
                    Action::OpenDetail => {
                        if !app.pipelines.is_empty() {
                            app.mode = Mode::Detail;
                        }
                    }
                    Action::Close => app.mode = Mode::Normal,
                    Action::None => {}
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    if args.once {
        run_once(&args.socket, args.no_color)
    } else {
        run_interactive(&args.socket, args.interval, args.no_color)
    }
}
