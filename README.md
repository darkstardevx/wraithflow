# 👻 WraithFlow

`Rust` · `Tokio` · `TCP Proxy` · `systemd`

**Stealth Traffic Network Proxy & Analyzer.** A small, config-driven TCP proxy —
each pipeline binds a `listen` address, forwards every byte to a `target`
address, and hex-dumps the traffic both ways as it flows through.

## 🚀 What it does

- Define any number of `[[proxies]]` pipelines in one config file
- Each pipeline is its own async task — one crashing doesn't take the others down
- Full-duplex byte-for-byte forwarding, no protocol awareness required
- Colorized hex-dump logging of every payload (optional, per pipeline)
- Fails fast on bad config (duplicate ports, bad addresses) before anything binds

## 🧩 Layout

```
wraithflow/            binary — loads config, spawns one task per pipeline
crates/wf-proxy/        library — the actual accept/forward/log loop
```

## ⚙️ Configuration

TOML by default, JSON still accepted by file extension for compatibility.
Resolved in this order:

1. `--config <path>` if passed
2. `$XDG_CONFIG_HOME/wraithflow/config.toml`
3. `~/.config/wraithflow/config.toml`
4. `./config.toml`, then `./config.json`, in the working directory

```toml
[[proxies]]
name = "http-traffic-gateway"
listen = "127.0.0.1:45634"
target = "127.0.0.1:9000"
enabled = true          # default true — set false to keep it defined but off
log_payloads = true     # default true — hex-dump traffic on this pipeline

[[proxies]]
name = "secure-db-relay"
listen = "127.0.0.1:3306"
target = "127.0.0.1:3307"
enabled = true
log_payloads = false    # off for anything carrying secrets — see below
```

> [!WARNING]
> `log_payloads` writes every byte that crosses a pipeline to stdout. Under
> systemd that lands in `journalctl` indefinitely. Turn it off for any
> pipeline carrying credentials, tokens, or other sensitive data.

## ▶️ Running

```bash
cargo build --release
./target/release/wraithflow                    # uses the resolved config
./target/release/wraithflow --config other.toml
```

### As a service

A `wraithflow.service` unit (not tracked here — lives in `/etc/systemd/system/`)
runs the release binary against `~/.config/wraithflow/config.toml`,
`Restart=on-failure`, enabled at boot.

```bash
systemctl status wraithflow
journalctl -u wraithflow -f
```

## 🗺 Roadmap

- [x] Config-driven multi-pipeline proxying
- [x] TOML config + XDG resolution
- [x] `enabled` / `log_payloads` per-pipeline toggles
- [x] Startup validation (duplicate ports, bad addresses)
- [x] systemd unit
- [ ] Structured/leveled logging (`tracing`) instead of `println!`
- [ ] Log file output with rotation (currently relies on journald)
- [ ] Graceful shutdown / connection draining on SIGTERM
- [ ] UDP pipeline support

## 📄 License

MIT
