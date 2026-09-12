use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use wf_core::{BufferPool, Direction, StatsRegistry};
use wf_packet::{OutputFormat, PacketFilter};
use wf_proxy::OutputSpec;

#[derive(Parser, Debug)]
#[command(name = "wraithflow", version = "0.1.0", about = "Stealth Traffic Network Proxy & Analyzer")]
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

/// Runs `systemctl <action> wraithflow`, `sudo`-prefixed for anything that
/// mutates service state. Inherits this process's stdio, so an interactive
/// sudo password prompt shows up exactly as if you'd typed the systemctl
/// command yourself.
fn run_admin(args: &Args) -> io::Result<i32> {
    let action = match (args.start, args.stop, args.restart, args.status) {
        (true, false, false, false) => "start",
        (false, true, false, false) => "stop",
        (false, false, true, false) => "restart",
        (false, false, false, true) => "status",
        (false, false, false, false) => {
            eprintln!("--admin needs exactly one of --start, --stop, --restart, --status");
            return Ok(1);
        }
        _ => {
            eprintln!("--admin takes exactly one of --start, --stop, --restart, --status, not several at once");
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

    println!("\x1b[35m[admin]\x1b[0m {:?}", cmd);
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(1))
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
    /// "hexdump" | "json" | "raw" | "base64"
    #[serde(default = "default_format")]
    format: String,
    /// Pretty-print JSON output. Ignored by other formats.
    #[serde(default)]
    pretty: bool,
    /// Syntax-highlight JSON, or colorize hexdump/raw/base64 output.
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
}

#[derive(Deserialize, Debug, Default)]
struct AppConfig {
    proxies: Vec<ProxyConfig>,
    #[serde(default)]
    output: Vec<OutputProfile>,
    /// How often to log a `[STATS]` summary per pipeline. 0 disables it.
    #[serde(default = "default_stats_interval")]
    stats_interval_secs: u64,
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
                "output profile \"{}\" has an unknown format \"{}\" (expected hexdump, json, raw, or base64)",
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
fn resolve_output(proxy: &ProxyConfig, profiles: &[OutputProfile]) -> OutputSpec {
    let profile = proxy.output.as_ref().and_then(|name| profiles.iter().find(|o| &o.name == name));

    let Some(profile) = profile else {
        return OutputSpec {
            enabled: proxy.log_payloads,
            ..OutputSpec::default()
        };
    };

    let direction = profile.direction.as_deref().and_then(|d| match d.to_ascii_lowercase().as_str() {
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
    println!("  {}» Stealth Traffic Network Proxy & Analyzer // v0.1.0{}", purple, reset);
    println!("  {}====================================================={}\n", teal, reset);
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.admin {
        let code = run_admin(&args)?;
        std::process::exit(code);
    }

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
            format!("Failed to open config file {}: {}", config_path.display(), e),
        )
    })?;

    let is_json = config_path.extension().and_then(|e| e.to_str()) == Some("json");
    let config: AppConfig = if is_json {
        serde_json::from_str(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Failed to parse JSON config: {}", e)))?
    } else {
        toml::from_str(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Failed to parse TOML config: {}", e)))?
    };

    validate(&config).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Config error: {}", e)))?;

    println!("\x1b[35m[Configuration Loaded]\x1b[0m {}", config_path.display());

    let enabled_count = config.proxies.iter().filter(|p| p.enabled).count();
    println!(
        "\x1b[35m[Configuration Processed]\x1b[0m Spawning {} independent pipelines ({} disabled)...",
        enabled_count,
        config.proxies.len() - enabled_count
    );

    let stats_registry = StatsRegistry::new();
    let buffer_pool = Arc::new(BufferPool::default());
    let mut tasks = vec![];

    // Iterate through the profiles and kick off a dedicated worker task for each one
    for proxy in config.proxies.into_iter().filter(|p| p.enabled) {
        let output = resolve_output(&proxy, &config.output);
        let stats = stats_registry.get_or_create(&proxy.name);
        let buffer_pool = buffer_pool.clone();

        let task = tokio::spawn(async move {
            println!("\x1b[32m[Spawning Worker]\x1b[0m Starting engine module: {}", proxy.name);
            if let Err(e) = wf_proxy::start_proxy(&proxy.name, &proxy.listen, &proxy.target, output, stats, buffer_pool).await {
                eprintln!("\x1b[31m[Critical Failure]\x1b[0m Engine error on [{}]: {}", proxy.name, e);
            }
        });
        tasks.push(task);
    }

    // A live, low-noise view of each pipeline: how many connections it's
    // handled, how many are open right now, how much data has moved each
    // way, and how many have errored (most commonly the target refusing
    // the connection). Set stats_interval_secs = 0 in the config to
    // disable.
    if config.stats_interval_secs > 0 {
        let registry = stats_registry.clone();
        let interval = config.stats_interval_secs;
        tasks.push(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval));
            loop {
                ticker.tick().await;
                for (name, snap) in registry.snapshot_all() {
                    println!(
                        "\x1b[34m[STATS]\x1b[0m {} — active={} total={} in={}B out={}B errors={}",
                        name, snap.connections_active, snap.connections_total, snap.bytes_in, snap.bytes_out, snap.errors_total
                    );
                }
            }
        }));
    }

    // Keep the runtime listening loop alive indefinitely across all routing components
    for task in tasks {
        let _ = task.await;
    }

    Ok(())
}
