use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Serialize;
use wf_core::Packet;

/// How a captured `Packet` gets turned into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// The original colorized hex + ASCII dump.
    Hexdump,
    /// A structured JSON record: pipeline, direction, timestamp, length,
    /// a best-effort UTF-8 decode, and the raw bytes as hex.
    Json,
    /// Lossy UTF-8 decode only — readable when the payload actually is text
    /// (HTTP, JSON APIs, plaintext protocols).
    Raw,
    /// Standard base64 of the raw bytes, one line.
    Base64,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hexdump" | "hex" => Some(Self::Hexdump),
            "json" => Some(Self::Json),
            "raw" | "text" => Some(Self::Raw),
            "base64" => Some(Self::Base64),
            _ => None,
        }
    }
}

#[derive(Serialize)]
struct PacketRecord<'a> {
    pipeline: &'a str,
    direction: &'static str,
    timestamp: String,
    length: usize,
    text: String,
    hex: String,
}

pub fn render(packet: &Packet, format: OutputFormat, pretty: bool, color: bool) -> String {
    match format {
        OutputFormat::Hexdump => render_hexdump(packet, color),
        OutputFormat::Json => render_json(packet, pretty, color),
        OutputFormat::Raw => render_raw(packet, color),
        OutputFormat::Base64 => render_base64(packet, color),
    }
}

fn render_hexdump(packet: &Packet, color: bool) -> String {
    let (c, reset) = if color {
        let c = if packet.direction == wf_core::Direction::Outbound {
            "\x1b[32m"
        } else {
            "\x1b[34m"
        };
        (c, "\x1b[0m")
    } else {
        ("", "")
    };

    let mut out = format!(
        "\n{}[{} Payload - {} bytes]{}\n",
        c,
        packet.direction.as_str(),
        packet.len(),
        reset
    );

    for chunk in packet.bytes.chunks(16) {
        let hex_string: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii_string: String = chunk
            .iter()
            .map(|&b| if (32..=126).contains(&b) { b as char } else { '.' })
            .collect();
        out.push_str(&format!("  {:48} | {}\n", hex_string.join(" "), ascii_string));
    }
    out
}

fn render_raw(packet: &Packet, color: bool) -> String {
    let text = String::from_utf8_lossy(&packet.bytes);
    if color {
        let c = if packet.direction == wf_core::Direction::Outbound {
            "\x1b[32m"
        } else {
            "\x1b[34m"
        };
        format!("{}[{}]\x1b[0m {}", c, packet.direction.as_str(), text)
    } else {
        format!("[{}] {}", packet.direction.as_str(), text)
    }
}

fn render_base64(packet: &Packet, color: bool) -> String {
    let encoded = BASE64.encode(&packet.bytes);
    if color {
        format!("\x1b[35m[{}]\x1b[0m {}", packet.direction.as_str(), encoded)
    } else {
        format!("[{}] {}", packet.direction.as_str(), encoded)
    }
}

fn render_json(packet: &Packet, pretty: bool, color: bool) -> String {
    let record = PacketRecord {
        pipeline: &packet.pipeline,
        direction: packet.direction.as_str(),
        timestamp: packet.timestamp.to_rfc3339(),
        length: packet.len(),
        text: String::from_utf8_lossy(&packet.bytes).into_owned(),
        hex: packet.bytes.iter().map(|b| format!("{:02x}", b)).collect(),
    };

    let value = match serde_json::to_value(&record) {
        Ok(v) => v,
        Err(e) => return format!("{{\"error\":\"failed to serialize packet: {}\"}}", e),
    };

    if color {
        // `to_colored_json_auto` pretty-prints with ANSI syntax highlighting
        // when stdout is a TTY, and falls back to plain compact JSON when
        // it's redirected (a log file, journald) — either way `pretty`
        // itself is honored below since compact JSON has no highlighting to
        // fall back to.
        match colored_json::to_colored_json_auto(&value) {
            Ok(s) => s,
            Err(_) => value.to_string(),
        }
    } else if pretty {
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}
