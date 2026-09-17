# Security Policy

## Threat model

What WraithFlow's design actually defends against, and what it
doesn't — read this before pointing a pipeline at anything that
matters.

### What's defended

- **No hidden listeners.** WraithFlow binds exactly the ports named in
  `[[proxies]]` — nothing implicit, nothing beyond what the operator
  explicitly configured. `validate()` (`src/main.rs`) rejects a config
  with duplicate listen ports or a bad address before anything binds,
  so a typo can't silently shadow another pipeline.
- **No credential handling.** WraithFlow is a byte-for-byte forwarder,
  not a protocol participant — it never parses, terminates, or
  originates TLS, HTTP auth, or any other credential exchange. It has
  nothing of its own to steal.
- **Sensitive traffic can be kept out of logs.** `log_payloads = false`
  turns off payload logging entirely for a pipeline (`secure-db-relay`
  in the example config ships this way by default, since raw MySQL
  wire traffic can carry credentials). Short of that, `redact` masks
  specific substrings with `*` before they're ever printed, and it
  runs *after* `contains` filtering — so a pipeline can gate logging on
  a secret's presence without that secret ever reaching the log
  itself.
- **The running unit shells out to nothing.** `wf-proxy`'s accept/
  forward loop (`crates/wf-proxy/src/lib.rs`) only opens TCP sockets
  and moves bytes — no subprocess of any kind. `--admin`'s `sudo
  systemctl` call is a separate CLI code path, invoked by a human at a
  terminal; it never runs inside the systemd unit's own `ExecStart`.
  That's what let `systemd/wraithflow.service` carry a real, fairly
  tight sandboxing profile (`NoNewPrivileges`, `ProtectSystem=strict`,
  `RestrictAddressFamilies=AF_INET AF_INET6`, `MemoryDenyWriteExecute`,
  an empty `CapabilityBoundingSet`, and more) without the kind of
  live conflict that forced ApexDaemon's hardening to be dropped
  almost entirely on this same host.

### What's NOT defended (by design)

- **Payload logging is not encrypted or access-controlled beyond
  journald's own permissions.** Anything actually logged (hexdump/
  json/raw/base64/compact) lands in `journalctl` indefinitely. Turning
  `log_payloads` off or `redact`-ing tightly for any pipeline carrying
  sensitive traffic is an **operator responsibility**, not something
  WraithFlow enforces automatically — there's no default redaction
  list, and a newly added pipeline defaults to `log_payloads = true`.
- **`--admin`'s `sudo systemctl` shortcut is a convenience wrapper, not
  a privilege boundary.** It prompts for the real password exactly as
  if you'd typed the `systemctl`/`sudo` command yourself — it doesn't
  grant, cache, or bypass anything.
- **The host it runs on.** Like every other daemon in this workspace,
  WraithFlow assumes the machine it runs on isn't already compromised.
  It doesn't defend against a local attacker with access to the
  running process, its config, or the journal it logs to.
- **Traffic between WraithFlow and its target is exactly as protected
  as it would be without WraithFlow in the middle.** WraithFlow adds
  no encryption of its own — if `target` is a plaintext service on
  localhost, the `listen → target` hop is exactly as plaintext as
  before. It's a proxy and observer, not a security boundary between
  client and target.

## Supported deployment model

A single user's own machine, proxying pipelines the operator
themselves configured and trusts, for local development/audit
purposes — the same "a handful of things you personally administer"
model as the rest of this workspace. Not designed to sit between an
untrusted client and a service you don't otherwise control.

## Reporting a vulnerability

Email **darkstardevx@gmail.com** (primary) or, as a backup,
**cybercore.sh@gmail.com**. Include:

- the affected file/commit and a minimal repro or PoC
- what you'd expect to happen instead
- how you'd rate the impact (your best guess is fine)

Expect an acknowledgement within a few days. Please don't include
exploit details in a public GitHub issue or PR until a fix has
shipped.

## Supported versions

Only the latest commit on `main` is supported — there's no tagged
release yet.
