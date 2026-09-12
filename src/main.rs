use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wraithflow", version = "0.1.0", about = "Stealth Traffic Network Proxy & Analyzer")]
struct Args {
    /// Path to the proxy routing config (.toml or .json). Defaults to
    /// $XDG_CONFIG_HOME/wraithflow/config.toml, then ~/.config/wraithflow/config.toml,
    /// then ./config.toml / ./config.json in the working directory.
    #[arg(short, long)]
    config: Option<PathBuf>,
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

// Map the config file structure directly into native Rust data objects
#[derive(Deserialize, Debug, Clone)]
struct ProxyConfig {
    name: String,
    listen: String,
    target: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    /// Hex-dump every payload that crosses this pipeline. Leave off for
    /// anything carrying credentials or sensitive data (e.g. a DB relay) —
    /// under systemd this ends up in journalctl indefinitely.
    #[serde(default = "default_log_payloads")]
    log_payloads: bool,
}

#[derive(Deserialize, Debug)]
struct AppConfig {
    proxies: Vec<ProxyConfig>,
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
    }
    Ok(())
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
    println!("\x1b[35m[Configuration Processed]\x1b[0m Spawning {} independent pipelines ({} disabled)...", enabled_count, config.proxies.len() - enabled_count);

    let mut tasks = vec![];

    // Iterate through the profiles and kick off a dedicated worker task for each one
    for proxy in config.proxies.into_iter().filter(|p| p.enabled) {
        let task = tokio::spawn(async move {
            println!("\x1b[32m[Spawning Worker]\x1b[0m Starting engine module: {}", proxy.name);
            if let Err(e) = wf_proxy::start_proxy(&proxy.name, &proxy.listen, &proxy.target, proxy.log_payloads).await {
                eprintln!("\x1b[31m[Critical Failure]\x1b[0m Engine error on [{}]: {}", proxy.name, e);
            }
        });
        tasks.push(task);
    }

    // Keep the runtime listening loop alive indefinitely across all routing components
    for task in tasks {
        let _ = task.await;
    }

    Ok(())
}
