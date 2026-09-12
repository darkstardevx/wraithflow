# 👻 WraithFlow

`Rust` · `Tokio` · `TCP Proxy` · `systemd`

**Stealth Traffic Network Proxy & Analyzer.** A small, config-driven TCP proxy —
each pipeline binds a `listen` address, forwards every byte to a `target`
address, and (optionally) captures the traffic both ways as it flows through:
hex-dumped, structured JSON, plain text, or base64, filterable so only the
packets you care about get rendered.

## 🚀 What it does

- Define any number of `[[proxies]]` pipelines in one config file
- Each pipeline is its own async task — one crashing doesn't take the others down
- Full-duplex byte-for-byte forwarding, no protocol awareness required
- Per-pipeline output profiles: hexdump / JSON (pretty + syntax-highlighted) / raw text / base64
- Filter what gets logged: minimum size, direction, substring match
- Live per-pipeline stats: connections, bytes each way, errors
- Fails fast on bad config (duplicate ports, bad addresses, dangling references) before anything binds

## 🧩 Layout

```
wraithflow/            binary — loads config, wires it all together
crates/wf-core/         shared primitives: Packet, Direction, BufferPool, stats registry
crates/wf-packet/       formatting (hexdump/json/raw/base64) + filtering, built on wf-core
crates/wf-proxy/        the accept/forward loop — uses wf-core + wf-packet, no formatting logic of its own
```

`wf-proxy` doesn't know what a hexdump or a JSON record looks like — it just
hands each chunk of payload to `wf-packet` as a `wf_core::Packet` and prints
whatever comes back. Formats live in one place (`wf-packet`), independent of
the forwarding logic.

## ⚙️ Configuration

TOML by default, JSON still accepted by file extension for compatibility.
Resolved in this order:

1. `--config <path>` if passed
2. `$XDG_CONFIG_HOME/wraithflow/config.toml`
3. `~/.config/wraithflow/config.toml`
4. `./config.toml`, then `./config.json`, in the working directory

```toml
stats_interval_secs = 30   # 0 disables the periodic [STATS] line

[[proxies]]
name = "http-traffic-gateway"
listen = "127.0.0.1:45634"
target = "127.0.0.1:9000"
enabled = true          # default true — set false to keep it defined but off
log_payloads = true     # default true — master on/off switch for logging
output = "json-pretty"  # references an [[output]] profile below

[[proxies]]
name = "secure-db-relay"
listen = "127.0.0.1:3306"
target = "127.0.0.1:3307"
enabled = true
log_payloads = false    # off for anything carrying secrets — see below

[[output]]
name = "json-pretty"
format = "json"         # "hexdump" | "json" | "raw" | "base64"
pretty = true
color = true            # syntax-highlight JSON / colorize hexdump-raw-base64
min_bytes = 0           # drop anything smaller than this
direction = ""          # "inbound" | "outbound" — omit for both
contains = ""           # only log packets whose bytes contain this substring
```

A `[[proxies]]` entry with no `output` gets a plain, unfiltered hexdump — the
original behavior. Output profiles are named and reusable, so several
pipelines can point at the same one.

> [!WARNING]
> Payload logging writes bytes that cross a pipeline to stdout. Under systemd
> that lands in `journalctl` indefinitely. Turn `log_payloads` off (or filter
> tightly) for any pipeline carrying credentials, tokens, or other sensitive
> data — `secure-db-relay` above ships muted by default for exactly this
> reason.

## 📊 Monitoring a pipeline

Every pipeline tracks, live, in `wf-core::PipelineStats`:

| Counter | What it tells you |
|---|---|
| `connections_active` | How many clients are connected through this pipeline right now |
| `connections_total` | How many have connected since the process started |
| `bytes_in` / `bytes_out` | Data volume in each direction (`in` = target → client, `out` = client → target) |
| `errors_total` | Failed connection attempts — almost always the *target* refusing the connection, not the listener |

These print as a `[STATS]` line every `stats_interval_secs` (default 30s),
per pipeline, whether or not payload logging is on — so you get throughput
and health visibility even on a muted pipeline like `secure-db-relay`.

**Reading it as someone newer to networking:** `listen` is the address
*clients connect to* — it's WraithFlow pretending to be the real service.
`target` is where WraithFlow actually forwards the traffic to — the real
service. A steadily climbing `errors_total` with `connections_active`
stuck at 0 almost always means the target isn't listening (wrong port, or
the real service is down) — WraithFlow accepted the client fine, then
failed to connect onward.

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

Or, since `wraithflow` itself is on `PATH` (symlinked into `~/.local/bin`),
skip remembering `systemctl`/`sudo` entirely:

```bash
wraithflow --admin --status    # no sudo needed
wraithflow --admin --start
wraithflow --admin --stop
wraithflow --admin --restart
```

These just shell out to `systemctl`/`sudo systemctl` with your stdio
attached — a real sudo prompt appears same as typing the command directly,
this is a shortcut, not a privilege change.

## 🗺 Roadmap

- [x] Config-driven multi-pipeline proxying
- [x] TOML config + XDG resolution
- [x] `enabled` / `log_payloads` per-pipeline toggles
- [x] Startup validation (duplicate ports, bad addresses, dangling output refs)
- [x] systemd unit
- [x] `wf-core` — shared `Packet`/`Direction` primitives, pooled buffers, stats registry
- [x] `wf-packet` — hexdump / JSON (pretty + colored) / raw / base64 output, packet filtering
- [x] Per-pipeline live stats (`connections`, `bytes`, `errors`)
- [ ] `wf-bpf` — optional eBPF kernel-space capture path (design TBD — needs root + a kernel-facing toolchain like `aya`; bigger scope than the userspace proxy, see the darknotes design note before starting)
- [ ] Structured/leveled logging (`tracing`) instead of `println!`
- [ ] Log file output with rotation (currently relies on journald)
- [ ] Graceful shutdown / connection draining on SIGTERM
- [ ] UDP pipeline support

## 📄 License

MIT
