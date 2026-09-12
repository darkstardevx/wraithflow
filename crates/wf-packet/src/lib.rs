//! Turns a raw `wf_core::Packet` into text: a classic hexdump, structured
//! JSON (pretty-printed and syntax-highlighted on request), plain decoded
//! text, or base64 — plus a small filter so only packets you care about get
//! rendered at all.

mod filter;
mod format;
mod redact;

pub use filter::PacketFilter;
pub use format::{render, OutputFormat};
pub use redact::Redactor;
