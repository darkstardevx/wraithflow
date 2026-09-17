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
crates/wf-packet/       formatting (hexdump/json/raw/base64/compact), filtering, redaction, and
                        highlighting — colors sourced from the shared `cybercore` CYBERGRID palette
crates/wf-proxy/        the accept/forward loop — uses wf-core + wf-packet, no formatting logic of its own
crates/wf-tui/          live dashboard over the control socket (see below) — a separate binary so the
                        daemon itself never carries ratatui/crossterm's dependency tree
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
format = "json"         # "hexdump" | "json" | "raw" | "base64" | "compact"
pretty = true
color = true            # syntax-highlight JSON / colorize the rest — from the
                         # shared cybercore CYBERGRID palette, not hardcoded
                         # ANSI. $CYBERGRID_THEME picks the theme, same as
                         # cyberdesk/cyberdeck (defaults to "neon-night").
min_bytes = 0           # drop anything smaller than this
direction = ""          # "inbound" | "outbound" — omit for both
contains = ""           # only log packets whose bytes contain this substring
redact = []             # mask matching substrings with `*` before logging —
                         # runs AFTER `contains` (which still sees real bytes)
highlight = []          # color-highlight matching substrings — "raw"/"compact"
                         # formats only; runs after redact
```

A `[[proxies]]` entry with no `output` gets a plain, unfiltered hexdump — the
original behavior. Output profiles are named and reusable, so several
pipelines can point at the same one.

**`redact` vs. `highlight`vs. `contains`**: all three take substrings, but do
different things. `contains` decides *whether* a packet gets logged at all
(and still sees the real, unredacted bytes). `redact` then masks matches
with `*` in what actually gets printed. `highlight` colors matches instead
of hiding them — for flagging things like error codes, not secrets — and
only affects `raw`/`compact` (hexdump/JSON/base64 have a fixed structure a
spliced-in color code would break).

> [!WARNING]
> Payload logging writes bytes that cross a pipeline to stdout. Under systemd
> that lands in `journalctl` indefinitely. Turn `log_payloads` off, filter
> tightly, or `redact` the sensitive parts for any pipeline carrying
> credentials, tokens, or other sensitive data — `secure-db-relay` above
> ships muted by default for exactly this reason.

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

The same counters are also available on demand, without scraping
`journalctl`, if `control_socket` is set in config.toml — a read-only
Unix socket for a future TUI or monitoring script to query:

```bash
echo '{"cmd":"stats"}' | socat - UNIX-CONNECT:/run/wraithflow/control.sock
# {"pipelines":[{"name":"http-traffic-gateway","bytes_in":0,"bytes_out":0,"connections_total":0,"connections_active":0,"errors_total":0}, ...]}
```

One request, one JSON response, connection closes — poll it by
reconnecting on whatever interval you need. Owner-only permissions
(`0600`) by default; disabled entirely unless `control_socket` is set.

That's exactly what `wf-tui` does — a live terminal dashboard over the
same socket. Symlinked into `~/.local/bin` the same way `wraithflow`
itself is, so no `cargo run -p` needed day to day:

```bash
wf-tui --socket /run/wraithflow/control.sock --interval 1
# or, from the repo: cargo run -p wf-tui -- --socket ... --interval ...
```

A table of every pipeline (active/total connections, bytes each way,
errors), refreshed every `--interval` seconds by reconnecting to the
socket, colored via the shared `cybercore` CYBERGRID palette the same
way `wf-packet`'s own output formats are (green = actively carrying
traffic, red = `errors_total > 0`, muted = idle and error-free).
`q`/`Esc` to quit. Requires `control_socket` to be set in
`config.toml` first — it has nothing to connect to otherwise.

**Reading it as someone newer to networking:** `listen` is the address
*clients connect to* — it's WraithFlow pretending to be the real service.
`target` is where WraithFlow actually forwards the traffic to — the real
service. A steadily climbing `errors_total` with `connections_active`
stuck at 0 almost always means the target isn't listening (wrong port, or
the real service is down) — WraithFlow accepted the client fine, then
failed to connect onward.

## 📋 Logging

Structured via [`tracing`](https://docs.rs/tracing) — `RUST_LOG` controls
verbosity (defaults to `info` if unset):

```bash
RUST_LOG=debug wraithflow   # also shows per-connection [+ Flow Connected]/
                             # [- Flow Disconnected] chatter, hidden by
                             # default now that there's a way to turn it
                             # down (previously always printed)
```

`info` (the default) covers pipeline lifecycle (startup, drain, errors)
and the periodic `[STATS]` line; `warn` covers a single connection's
recoverable error (target refused, etc.); `debug` adds the per-connection
churn. `--admin`'s own output (`--admin --status` etc.) is unaffected —
that's direct command output to whoever's running it, not a log line.

## 🛑 Graceful shutdown

On SIGTERM (`systemctl stop`/`restart`) or Ctrl+C, WraithFlow stops
accepting *new* connections on every pipeline immediately, then waits up
to `shutdown_drain_secs` (default 8, config.toml) for connections already
in flight to finish naturally before exiting — a restart or stop no
longer hard-resets every open connection instantly. Keep
`shutdown_drain_secs` comfortably under `systemd/wraithflow.service`'s
`TimeoutStopSec` (currently 10), or systemd's own SIGKILL cuts the drain
short before WraithFlow's own timeout gets a chance to.

## ▶️ Running

```bash
cargo build --release
./target/release/wraithflow                    # uses the resolved config
./target/release/wraithflow --config other.toml
```

### Contributing

One-time setup to run the fast gates (fmt/clippy/check) automatically
before every commit:

```bash
git config core.hooksPath .githooks
```

`./scripts/release-gates full` (fmt/clippy/check/test/`cargo deny
check`) is the full gate set — run it before pushing.

### As a service

`systemd/wraithflow.service` runs the release binary against
`~/.config/wraithflow/config.toml`, `Restart=on-failure`, enabled at boot,
with a real systemd sandboxing profile (see [SECURITY.md](SECURITY.md)).
Install it with:

```bash
sudo cp systemd/wraithflow.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now wraithflow
```

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
- [x] `wf-packet` — hexdump / JSON (pretty + colored) / raw / base64 / compact output, packet filtering
- [x] Per-pipeline live stats (`connections`, `bytes`, `errors`)
- [x] `--admin --start/--stop/--restart/--status` service control, no `systemctl` incantation needed
- [x] `redact` (mask secrets) and `highlight` (flag patterns) on output profiles
- [x] Colors sourced from the shared `cybercore` CYBERGRID palette instead of hardcoded ANSI
- [x] Read-only Unix control socket for live stats (`control_socket`) — a foundation for a future TUI
- [x] `wf-tui` — live dashboard consuming the control socket
- [x] Structured/leveled logging (`tracing`) instead of `println!` — `RUST_LOG` controls verbosity (see below)
- [x] Graceful shutdown / connection draining on SIGTERM (and Ctrl+C) — `shutdown_drain_secs`
- [ ] `wf-bpf` — optional eBPF kernel-space capture path (design TBD — needs root + a kernel-facing toolchain like `aya`; bigger scope than the userspace proxy, see the darknotes design note before starting)
- [ ] Log file output with rotation (currently relies on journald)
- [ ] UDP pipeline support

## 📄 License

MIT
