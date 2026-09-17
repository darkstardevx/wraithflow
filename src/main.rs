mod control;

use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use wf_core::{BufferPool, Direction, StatsRegistry};
use wf_packet::{OutputFormat, PacketFilter};
use wf_proxy::OutputSpec;

#[derive(Parser, Debug)]
#[command(
    name = "wraithflow",
    version = "0.1.0",
    about = "Stealth Traffic Network Proxy & Analyzer"
)]
struct Args {
    /// Path to the proxy routing config (.toml or .json). Defaults to
    /// $XDG_CONFIG_HOME/wraithflow/config.toml, then ~/.config/wraithflow/config.toml,
    /// then ./config.toml / ./config.json in the working directory.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Manage the wraithflow systemd service instead of running the proxy
    /// directly. Combine with exactly one of --start/--stop/--restart/--status.
    /// start/stop/restart shell out to `sudo systemctl` and prompt for your
    /// password same as typing it yourself; status doesn't need sudo.
    #[arg(long)]
    admin: bool,

    #[arg(long, requires = "admin")]
    start: bool,
    #[arg(long, requires = "admin")]
    stop: bool,
    #[arg(long, requires = "admin")]
    restart: bool,
    #[arg(long, requires = "admin")]
    status: bool,
}

/// Exactly one of `start`/`stop`/`restart`/`status` must be set. Split out
/// from `run_admin` so this selection logic is testable without touching
/// `Command`/process spawning.
fn resolve_admin_action(
    start: bool,
    stop: bool,
    restart: bool,
    status: bool,
) -> Result<&'static str, &'static str> {
    match (start, stop, restart, status) {
        (true, false, false, false) => Ok("start"),
        (false, true, false, false) => Ok("stop"),
        (false, false, true, false) => Ok("restart"),
        (false, false, false, true) => Ok("status"),
        (false, false, false, false) => {
            Err("--admin needs exactly one of --start, --stop, --restart, --status")
        }
        _ => Err("--admin takes exactly one of --start, --stop, --restart, --status, not several at once"),
    }
}

/// Runs `systemctl <action> wraithflow`, `sudo`-prefixed for anything that
/// mutates service state. Inherits this process's stdio, so an interactive
/// sudo password prompt shows up exactly as if you'd typed the systemctl
/// command yourself.
fn run_admin(args: &Args) -> io::Result<i32> {
    let action = match resolve_admin_action(args.start, args.stop, args.restart, args.status) {
        Ok(action) => action,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(1);
        }
    };

    let mut cmd = if action == "status" {
        let mut c = std::process::Command::new("systemctl");
        c.arg("status");
        c
    } else {
        let mut c = std::process::Command::new("sudo");
        c.args(["systemctl", action]);
        c
    };
    cmd.arg("wraithflow");

    // Plain text, explicitly flushed, before handing the terminal to sudo's
    // own interactive password prompt: color codes and unflushed buffering
    // here have been observed to garble that handoff under at least one
    // terminal/pty bridge (leftover bytes getting fed back to the shell as
    // a bogus follow-up command). Keep this boring on purpose.
    let prefix = if action == "status" { "" } else { "sudo " };
    println!("[admin] running: {}systemctl {} wraithflow", prefix, action);
    io::stdout().flush()?;
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(1))
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn default_config_path() -> Option<PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok()?;

    let candidates = [
        config_home.join("wraithflow").join("config.toml"),
        PathBuf::from("config.toml"),
        PathBuf::from("config.json"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn default_log_payloads() -> bool {
    true
}

fn default_enabled() -> bool {
    true
}

fn default_format() -> String {
    "hexdump".to_string()
}

fn default_color() -> bool {
    true
}

fn default_stats_interval() -> u64 {
    30
}

fn default_shutdown_drain_secs() -> u64 {
    8
}

// Map the config file structure directly into native Rust data objects
#[derive(Deserialize, Debug, Clone)]
struct ProxyConfig {
    name: String,
    listen: String,
    target: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    /// Master on/off switch for payload logging on this pipeline. Leave off
    /// for anything carrying credentials or sensitive data (e.g. a DB
    /// relay) — under systemd this ends up in journalctl indefinitely.
    #[serde(default = "default_log_payloads")]
    log_payloads: bool,
    /// Name of an `[[output]]` profile to use for formatting/filtering.
    /// `None` = a plain hexdump with no filter (the original behavior).
    #[serde(default)]
    output: Option<String>,
}

/// A named, reusable formatting + filtering profile, referenced from
/// `[[proxies]]` by `output = "<name>"`. Splitting this out (instead of
/// inlining format/filter fields on every proxy) means one profile can be
/// shared across several pipelines, the same way `[[proxies]]` itself is a
/// named, repeatable block.
#[derive(Deserialize, Debug, Clone)]
struct OutputProfile {
    name: String,
    /// "hexdump" | "json" | "raw" | "base64" | "compact"
    #[serde(default = "default_format")]
    format: String,
    /// Pretty-print JSON output. Ignored by other formats.
    #[serde(default)]
    pretty: bool,
    /// Syntax-highlight JSON, or colorize hexdump/raw/base64/compact output.
    /// Colors come from the shared cybercore CYBERGRID palette
    /// ($CYBERGRID_THEME picks which theme, same as cyberdesk/cyberdeck).
    #[serde(default = "default_color")]
    color: bool,
    /// Drop packets smaller than this many bytes.
    #[serde(default)]
    min_bytes: usize,
    /// Only log one direction: "inbound" | "outbound". Omit for both.
    #[serde(default)]
    direction: Option<String>,
    /// Only log packets whose bytes contain this substring.
    #[serde(default)]
    contains: Option<String>,
    /// Mask every occurrence of these substrings with `*` before logging.
    /// Runs after `contains` filtering (which still sees the real bytes) —
    /// so you can gate on a secret's presence without printing the secret.
    #[serde(default)]
    redact: Vec<String>,
    /// Color-highlight matches of these substrings in the output. Only
    /// applies to "raw" and "compact" formats (hexdump/json/base64 have a
    /// fixed structure a spliced-in color code would corrupt). Runs after
    /// redact, so a highlighted match can't reveal a redacted one.
    #[serde(default)]
    highlight: Vec<String>,
}

#[derive(Deserialize, Debug, Default)]
struct AppConfig {
    proxies: Vec<ProxyConfig>,
    #[serde(default)]
    output: Vec<OutputProfile>,
    /// How often to log a `[STATS]` summary per pipeline. 0 disables it.
    #[serde(default = "default_stats_interval")]
    stats_interval_secs: u64,
    /// Path for a read-only Unix control socket exposing live stats as
    /// JSON (see `control.rs`). Omit to disable -- no implicit default
    /// path, no new attack surface unless explicitly opted into.
    #[serde(default)]
    control_socket: Option<String>,
    /// On SIGTERM/Ctrl+C, how long to wait for in-flight connections to
    /// finish naturally before exiting anyway. Keep this comfortably
    /// under systemd/wraithflow.service's TimeoutStopSec (currently 10)
    /// -- otherwise systemd's own SIGKILL cuts the drain short before
    /// this timeout ever gets a chance to.
    #[serde(default = "default_shutdown_drain_secs")]
    shutdown_drain_secs: u64,
    /// Path for an optional shared JSONL capture log -- every logged
    /// packet across every pipeline, one file, not per-pipeline. Omit to
    /// disable (today's stdout-only behavior, unchanged). Exists so a
    /// separate tool (Echo) can browse captured traffic instead of only
    /// ever seeing it scroll past in `journalctl -u wraithflow`.
    #[serde(default)]
    capture_log: Option<String>,
}

/// Fail fast on config problems that would otherwise surface one task at a
/// time as opaque bind errors after the banner's already printed.
fn validate(config: &AppConfig) -> Result<(), String> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for proxy in config.proxies.iter().filter(|p| p.enabled) {
        if let Some(existing) = seen.insert(proxy.listen.as_str(), proxy.name.as_str()) {
            return Err(format!(
                "pipelines \"{}\" and \"{}\" both listen on {}",
                existing, proxy.name, proxy.listen
            ));
        }
        if proxy.listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(format!(
                "pipeline \"{}\" has an invalid listen address: {}",
                proxy.name, proxy.listen
            ));
        }
        if let Some(output_name) = &proxy.output {
            if !config.output.iter().any(|o| &o.name == output_name) {
                return Err(format!(
                    "pipeline \"{}\" references output profile \"{}\", which doesn't exist",
                    proxy.name, output_name
                ));
            }
        }
    }
    for profile in &config.output {
        if OutputFormat::parse(&profile.format).is_none() {
            return Err(format!(
                "output profile \"{}\" has an unknown format \"{}\" (expected hexdump, json, raw, base64, or compact)",
                profile.name, profile.format
            ));
        }
        if let Some(dir) = &profile.direction {
            if !matches!(dir.to_ascii_lowercase().as_str(), "inbound" | "outbound") {
                return Err(format!(
                    "output profile \"{}\" has an unknown direction \"{}\" (expected inbound or outbound)",
                    profile.name, dir
                ));
            }
        }
    }
    Ok(())
}

/// Turn a named `[[output]]` profile (or the absence of one) into the
/// `OutputSpec` `wf-proxy` actually runs with.
fn resolve_output(
    proxy: &ProxyConfig,
    profiles: &[OutputProfile],
    capture_log: Option<Arc<wf_proxy::CaptureLog>>,
) -> OutputSpec {
    let profile = proxy
        .output
        .as_ref()
        .and_then(|name| profiles.iter().find(|o| &o.name == name));

    let Some(profile) = profile else {
        return OutputSpec {
            enabled: proxy.log_payloads,
            capture_log,
            ..OutputSpec::default()
        };
    };

    let direction =
        profile
            .direction
            .as_deref()
            .and_then(|d| match d.to_ascii_lowercase().as_str() {
                "inbound" => Some(Direction::Inbound),
                "outbound" => Some(Direction::Outbound),
                _ => None,
            });

    OutputSpec {
        enabled: proxy.log_payloads,
        format: OutputFormat::parse(&profile.format).unwrap_or(OutputFormat::Hexdump),
        pretty: profile.pretty,
        color: profile.color,
        filter: PacketFilter {
            min_bytes: profile.min_bytes,
            direction,
            contains: profile.contains.as_ref().map(|s| s.as_bytes().to_vec()),
        },
        redact: wf_packet::Redactor::new(&profile.redact),
        highlight: profile
            .highlight
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_bytes().to_vec())
            .collect(),
        capture_log,
    }
}

fn print_banner() {
    let teal = "\x1b[36m";
    let purple = "\x1b[35m";
    let reset = "\x1b[0m";
    let bold = "\x1b[1m";

    let ascii_art = r#"
 __      __           _ _   _     ______ _
 \ \    / /          (_) | | |   |  ____| |
  \ \  / / __ __ _ _ _| |_| |__  | |__  | | _____      __
   \ \/ / '__/ _` | | | __| '_ \ |  __| | |/ _ \ \ /\ / /
    \  /| | | (_| | | | |_| | | || |    | | (_) \ V  V /
     \/ |_|  \__,_|_|_|\__|_| |_||_|    |_|\___/ \_/\_/    "#;

    println!("{}{}{}", bold, teal, ascii_art);
    println!(
        "  {}» Stealth Traffic Network Proxy & Analyzer // v0.1.0{}",
        purple, reset
    );
    println!(
        "  {}====================================================={}\n",
        teal, reset
    );
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.admin {
        let code = run_admin(&args)?;
        std::process::exit(code);
    }

    // journald (under systemd) already timestamps every line, and the
    // target module path tracing adds by default is redundant with the
    // existing [pipeline-name]/[Configuration] bracket tags every
    // message already carries -- both are turned off so tracing only
    // adds levels/filtering, not a second, competing log style.
    // Messages keep their existing manually-embedded cybercore ANSI
    // codes, so the formatter's own coloring is left off too.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_ansi(false)
        .with_env_filter(env_filter)
        .init();

    print_banner();

    let config_path = args.config.or_else(default_config_path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "No config found (checked --config, $XDG_CONFIG_HOME/wraithflow, ~/.config/wraithflow, ./config.toml, ./config.json)",
        )
    })?;

    let raw = fs::read_to_string(&config_path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Failed to open config file {}: {}",
                config_path.display(),
                e
            ),
        )
    })?;

    let is_json = config_path.extension().and_then(|e| e.to_str()) == Some("json");
    let config: AppConfig = if is_json {
        serde_json::from_str(&raw).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse JSON config: {}", e),
            )
        })?
    } else {
        toml::from_str(&raw).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse TOML config: {}", e),
            )
        })?
    };

    validate(&config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Config error: {}", e)))?;

    tracing::info!(
        "\x1b[35m[Configuration Loaded]\x1b[0m {}",
        config_path.display()
    );

    let enabled_count = config.proxies.iter().filter(|p| p.enabled).count();
    tracing::info!(
        "\x1b[35m[Configuration Processed]\x1b[0m Spawning {} independent pipelines ({} disabled)...",
        enabled_count,
        config.proxies.len() - enabled_count
    );

    // Best-effort, not fail-fast like `validate()` above -- a capture log
    // that can't be opened (bad permissions, disk full) shouldn't stop
    // the proxy itself from forwarding traffic, which is the actual job.
    let capture_log: Option<Arc<wf_proxy::CaptureLog>> =
        config.capture_log.as_deref().and_then(|path| {
            match wf_proxy::CaptureLog::open(&expand_home(path)) {
                Ok(log) => {
                    tracing::info!("\x1b[35m[Capture Log]\x1b[0m Writing captures to {path}");
                    Some(Arc::new(log))
                }
                Err(e) => {
                    tracing::warn!(
                        "[Capture Log] could not open {path}: {e} -- continuing without it"
                    );
                    None
                }
            }
        });

    let shutdown = wf_proxy::Shutdown {
        token: CancellationToken::new(),
        tracker: TaskTracker::new(),
    };

    {
        let token = shutdown.token.clone();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            tracing::info!("\x1b[35m[Shutdown]\x1b[0m Draining in-flight connections...");
            token.cancel();
        });
    }

    let stats_registry = StatsRegistry::new();
    let buffer_pool = Arc::new(BufferPool::default());
    let mut pipeline_tasks = vec![];

    // Iterate through the profiles and kick off a dedicated worker task for each one
    for proxy in config.proxies.into_iter().filter(|p| p.enabled) {
        let output = resolve_output(&proxy, &config.output, capture_log.clone());
        let stats = stats_registry.get_or_create(&proxy.name);
        let buffer_pool = buffer_pool.clone();
        let shutdown = shutdown.clone();

        let task = tokio::spawn(async move {
            tracing::info!(
                "\x1b[32m[Spawning Worker]\x1b[0m Starting engine module: {}",
                proxy.name
            );
            if let Err(e) = wf_proxy::start_proxy(
                &proxy.name,
                &proxy.listen,
                &proxy.target,
                output,
                stats,
                buffer_pool,
                shutdown,
            )
            .await
            {
                tracing::error!(
                    "\x1b[31m[Critical Failure]\x1b[0m Engine error on [{}]: {}",
                    proxy.name,
                    e
                );
            }
        });
        pipeline_tasks.push(task);
    }

    // A live, low-noise view of each pipeline: how many connections it's
    // handled, how many are open right now, how much data has moved each
    // way, and how many have errored (most commonly the target refusing
    // the connection). Set stats_interval_secs = 0 in the config to
    // disable.
    let stats_handle = if config.stats_interval_secs > 0 {
        let registry = stats_registry.clone();
        let interval = config.stats_interval_secs;
        Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval));
            loop {
                ticker.tick().await;
                for (name, snap) in registry.snapshot_all() {
                    tracing::info!(
                        "\x1b[34m[STATS]\x1b[0m {} — active={} total={} in={}B out={}B errors={}",
                        name,
                        snap.connections_active,
                        snap.connections_total,
                        snap.bytes_in,
                        snap.bytes_out,
                        snap.errors_total
                    );
                }
            }
        }))
    } else {
        None
    };

    let control_handle = if let Some(path) = config.control_socket {
        let stats = stats_registry.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = control::serve(std::path::Path::new(&path), stats).await {
                tracing::error!(
                    "\x1b[31m[control]\x1b[0m Failed to serve on {}: {}",
                    path,
                    e
                );
            }
        }))
    } else {
        None
    };

    // Every pipeline's accept loop returns once `shutdown` is cancelled
    // (or on a real bind error) -- this only unblocks once all of them
    // have stopped taking new connections.
    for task in pipeline_tasks {
        let _ = task.await;
    }

    // Stop counting new sessions into the tracker, then bound how long
    // we wait for the ones already in flight -- comfortably under
    // systemd/wraithflow.service's TimeoutStopSec so systemd's own
    // SIGKILL never has to be the thing that cuts a session off.
    shutdown.tracker.close();
    tokio::select! {
        () = shutdown.tracker.wait() => {
            tracing::info!("\x1b[35m[Shutdown]\x1b[0m All sessions drained cleanly");
        }
        () = tokio::time::sleep(Duration::from_secs(config.shutdown_drain_secs)) => {
            tracing::warn!(
                "\x1b[33m[Shutdown]\x1b[0m Drain timeout ({}s) reached, exiting with sessions still in flight",
                config.shutdown_drain_secs
            );
        }
    }

    // Neither of these holds client connections needing a graceful
    // close -- killing them outright is harmless once every pipeline
    // has already stopped accepting and drained.
    if let Some(h) = stats_handle {
        h.abort();
    }
    if let Some(h) = control_handle {
        h.abort();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(name: &str, listen: &str, target: &str) -> ProxyConfig {
        ProxyConfig {
            name: name.to_string(),
            listen: listen.to_string(),
            target: target.to_string(),
            enabled: true,
            log_payloads: true,
            output: None,
        }
    }

    fn profile(name: &str, format: &str) -> OutputProfile {
        OutputProfile {
            name: name.to_string(),
            format: format.to_string(),
            pretty: false,
            color: false,
            min_bytes: 0,
            direction: None,
            contains: None,
            redact: vec![],
            highlight: vec![],
        }
    }

    #[test]
    fn resolve_admin_action_maps_each_single_flag() {
        assert_eq!(resolve_admin_action(true, false, false, false), Ok("start"));
        assert_eq!(resolve_admin_action(false, true, false, false), Ok("stop"));
        assert_eq!(
            resolve_admin_action(false, false, true, false),
            Ok("restart")
        );
        assert_eq!(
            resolve_admin_action(false, false, false, true),
            Ok("status")
        );
    }

    #[test]
    fn resolve_admin_action_rejects_no_flags_and_multiple_flags() {
        assert!(resolve_admin_action(false, false, false, false).is_err());
        assert!(resolve_admin_action(true, true, false, false).is_err());
    }

    #[test]
    fn validate_accepts_a_normal_config() {
        let config = AppConfig {
            proxies: vec![proxy("a", "127.0.0.1:1000", "127.0.0.1:2000")],
            output: vec![],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn validate_rejects_duplicate_listen_ports_among_enabled_proxies() {
        let config = AppConfig {
            proxies: vec![
                proxy("a", "127.0.0.1:1000", "127.0.0.1:2000"),
                proxy("b", "127.0.0.1:1000", "127.0.0.1:3000"),
            ],
            output: vec![],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_ignores_a_disabled_proxy_sharing_a_port() {
        let mut disabled = proxy("b", "127.0.0.1:1000", "127.0.0.1:3000");
        disabled.enabled = false;
        let config = AppConfig {
            proxies: vec![proxy("a", "127.0.0.1:1000", "127.0.0.1:2000"), disabled],
            output: vec![],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn validate_rejects_an_invalid_listen_address() {
        let config = AppConfig {
            proxies: vec![proxy("a", "not-an-address", "127.0.0.1:2000")],
            output: vec![],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_a_dangling_output_reference() {
        let mut p = proxy("a", "127.0.0.1:1000", "127.0.0.1:2000");
        p.output = Some("missing".to_string());
        let config = AppConfig {
            proxies: vec![p],
            output: vec![],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_an_unknown_output_format() {
        let config = AppConfig {
            proxies: vec![],
            output: vec![profile("p", "not-a-format")],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_an_unknown_direction() {
        let mut p = profile("p", "json");
        p.direction = Some("sideways".to_string());
        let config = AppConfig {
            proxies: vec![],
            output: vec![p],
            stats_interval_secs: 30,
            control_socket: None,
            shutdown_drain_secs: 8,
            capture_log: None,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn resolve_output_with_no_profile_uses_log_payloads_and_defaults() {
        let mut p = proxy("a", "127.0.0.1:1000", "127.0.0.1:2000");
        p.log_payloads = false;
        let output = resolve_output(&p, &[], None);
        assert!(!output.enabled);
        assert_eq!(output.format, OutputFormat::Hexdump);
    }

    #[test]
    fn resolve_output_maps_a_named_profiles_fields() {
        let mut p = proxy("a", "127.0.0.1:1000", "127.0.0.1:2000");
        p.output = Some("prof".to_string());
        let mut prof = profile("prof", "json");
        prof.pretty = true;
        prof.min_bytes = 10;
        prof.direction = Some("outbound".to_string());
        prof.contains = Some("GET".to_string());

        let output = resolve_output(&p, &[prof], None);
        assert_eq!(output.format, OutputFormat::Json);
        assert!(output.pretty);
        assert_eq!(output.filter.min_bytes, 10);
        assert_eq!(output.filter.direction, Some(Direction::Outbound));
        assert_eq!(output.filter.contains, Some(b"GET".to_vec()));
    }

    #[test]
    fn resolve_output_falls_back_to_both_directions_on_an_unknown_string() {
        let mut p = proxy("a", "127.0.0.1:1000", "127.0.0.1:2000");
        p.output = Some("prof".to_string());
        let mut prof = profile("prof", "json");
        prof.direction = Some("sideways".to_string());

        let output = resolve_output(&p, &[prof], None);
        assert_eq!(output.filter.direction, None);
    }
}
