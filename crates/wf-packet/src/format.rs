use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Serialize;
use wf_core::{Direction, Packet};

const COMPACT_PREVIEW_LEN: usize = 60;

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
    /// One line per packet: direction, length, and a truncated preview.
    /// For scanning a busy pipeline at a glance instead of reading every
    /// full payload.
    Compact,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hexdump" | "hex" => Some(Self::Hexdump),
            "json" => Some(Self::Json),
            "raw" | "text" => Some(Self::Raw),
            "base64" => Some(Self::Base64),
            "compact" | "summary" => Some(Self::Compact),
            _ => None,
        }
    }
}

/// The two CYBERGRID roles used for direction — picked to read the same
/// way the original hardcoded green/blue did (outbound = "going, active",
/// inbound = "arriving, informational"), just sourced from the shared
/// palette instead of copied hex/ANSI values.
fn direction_color(direction: Direction) -> String {
    match direction {
        Direction::Outbound => cybercore::palette::acid_green(),
        Direction::Inbound => cybercore::palette::cyan(),
    }
}

fn reset() -> &'static str {
    cybercore::palette::RESET
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

pub fn render(
    packet: &Packet,
    format: OutputFormat,
    pretty: bool,
    color: bool,
    highlight: &[Vec<u8>],
) -> String {
    match format {
        OutputFormat::Hexdump => render_hexdump(packet, color),
        OutputFormat::Json => render_json(packet, pretty, color),
        OutputFormat::Raw => render_raw(packet, color, highlight),
        OutputFormat::Base64 => render_base64(packet, color),
        OutputFormat::Compact => render_compact(packet, color, highlight),
    }
}

/// Splices ANSI highlight codes around every match of every pattern,
/// leftmost pattern in the list wins on overlap. Only meaningful for the
/// text formats (`Raw`/`Compact`) — hexdump/JSON/base64 have their own
/// fixed structure a spliced-in escape code would corrupt.
fn apply_highlight(bytes: &[u8], patterns: &[Vec<u8>]) -> Vec<u8> {
    if patterns.is_empty() {
        return bytes.to_vec();
    }
    let color = cybercore::palette::red().into_bytes();
    let reset = reset().as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hit = patterns.iter().find(|p| {
            let plen = p.len();
            plen > 0 && i + plen <= bytes.len() && bytes[i..i + plen] == p[..]
        });
        match hit {
            Some(pattern) => {
                out.extend_from_slice(&color);
                out.extend_from_slice(&bytes[i..i + pattern.len()]);
                out.extend_from_slice(reset);
                i += pattern.len();
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

fn render_hexdump(packet: &Packet, color: bool) -> String {
    let (c, r) = if color {
        (direction_color(packet.direction), reset().to_string())
    } else {
        (String::new(), String::new())
    };

    let mut out = format!(
        "\n{}[{} Payload - {} bytes]{}\n",
        c,
        packet.direction.as_str(),
        packet.len(),
        r
    );

    for chunk in packet.bytes.chunks(16) {
        let hex_string: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii_string: String = chunk
            .iter()
            .map(|&b| {
                if (32..=126).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "  {:48} | {}\n",
            hex_string.join(" "),
            ascii_string
        ));
    }
    out
}

fn render_raw(packet: &Packet, color: bool, highlight: &[Vec<u8>]) -> String {
    let display_bytes = if color {
        apply_highlight(&packet.bytes, highlight)
    } else {
        packet.bytes.clone()
    };
    let text = String::from_utf8_lossy(&display_bytes);
    if color {
        format!(
            "{}[{}]{} {}",
            direction_color(packet.direction),
            packet.direction.as_str(),
            reset(),
            text
        )
    } else {
        format!("[{}] {}", packet.direction.as_str(), text)
    }
}

fn render_compact(packet: &Packet, color: bool, highlight: &[Vec<u8>]) -> String {
    let display_bytes = if color {
        apply_highlight(&packet.bytes, highlight)
    } else {
        packet.bytes.clone()
    };
    let mut text: String = String::from_utf8_lossy(&display_bytes)
        .chars()
        .map(|c| if c.is_control() { '.' } else { c })
        .collect();
    if text.chars().count() > COMPACT_PREVIEW_LEN {
        text = text.chars().take(COMPACT_PREVIEW_LEN).collect::<String>() + "…";
    }
    if color {
        format!(
            "{}[{}]{} {}B \"{}\"",
            direction_color(packet.direction),
            packet.direction.as_str(),
            reset(),
            packet.len(),
            text
        )
    } else {
        format!(
            "[{}] {}B \"{}\"",
            packet.direction.as_str(),
            packet.len(),
            text
        )
    }
}

fn render_base64(packet: &Packet, color: bool) -> String {
    let encoded = BASE64.encode(&packet.bytes);
    if color {
        format!(
            "{}[{}]{} {}",
            cybercore::palette::purple(),
            packet.direction.as_str(),
            reset(),
            encoded
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use wf_core::Direction;

    #[test]
    fn parse_recognizes_every_alias_case_insensitively() {
        assert_eq!(OutputFormat::parse("HEX"), Some(OutputFormat::Hexdump));
        assert_eq!(OutputFormat::parse("hexdump"), Some(OutputFormat::Hexdump));
        assert_eq!(OutputFormat::parse("Json"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("text"), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::parse("raw"), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::parse("BASE64"), Some(OutputFormat::Base64));
        assert_eq!(OutputFormat::parse("summary"), Some(OutputFormat::Compact));
        assert_eq!(OutputFormat::parse("compact"), Some(OutputFormat::Compact));
        assert_eq!(OutputFormat::parse("bogus"), None);
    }

    #[test]
    fn apply_highlight_is_a_noop_with_no_patterns() {
        let bytes = b"hello world".to_vec();
        assert_eq!(apply_highlight(&bytes, &[]), bytes);
    }

    #[test]
    fn apply_highlight_wraps_every_match_and_leaves_the_rest_untouched() {
        let out = apply_highlight(b"the secret word", &[b"secret".to_vec()]);
        let text = String::from_utf8(out).unwrap();
        let color = cybercore::palette::red();
        let reset = cybercore::palette::RESET;
        assert_eq!(text, format!("the {color}secret{reset} word"));
    }

    #[test]
    fn apply_highlight_leftmost_pattern_wins_on_overlap() {
        // Both patterns match starting at the same byte -- "ab" (listed
        // first) should win over "abc".
        let out = apply_highlight(b"abcdef", &[b"ab".to_vec(), b"abc".to_vec()]);
        let text = String::from_utf8(out).unwrap();
        let color = cybercore::palette::red();
        let reset = cybercore::palette::RESET;
        assert_eq!(text, format!("{color}ab{reset}cdef"));
    }

    #[test]
    fn render_compact_truncates_long_text_with_an_ellipsis() {
        let packet = Packet::new("p", Direction::Outbound, &[b'x'; 100]);
        let out = render_compact(&packet, false, &[]);
        assert!(out.contains('…'));
        assert!(out.contains(&"x".repeat(COMPACT_PREVIEW_LEN)));
        assert!(!out.contains(&"x".repeat(COMPACT_PREVIEW_LEN + 1)));
    }

    #[test]
    fn render_compact_replaces_control_characters_with_a_dot() {
        let packet = Packet::new("p", Direction::Inbound, b"a\nb\tc");
        let out = render_compact(&packet, false, &[]);
        assert_eq!(out, "[INBOUND] 5B \"a.b.c\"");
    }

    #[test]
    fn render_compact_leaves_short_text_unchanged() {
        let packet = Packet::new("p", Direction::Outbound, b"hi");
        let out = render_compact(&packet, false, &[]);
        assert_eq!(out, "[OUTBOUND] 2B \"hi\"");
    }

    #[test]
    fn render_raw_without_color_is_the_direction_tag_plus_lossy_utf8() {
        let packet = Packet::new("p", Direction::Outbound, b"GET / HTTP/1.1");
        assert_eq!(render_raw(&packet, false, &[]), "[OUTBOUND] GET / HTTP/1.1");
    }

    #[test]
    fn render_base64_without_color_encodes_the_raw_bytes() {
        let packet = Packet::new("p", Direction::Inbound, b"hi");
        assert_eq!(render_base64(&packet, false), "[INBOUND] aGk=");
    }

    #[test]
    fn render_json_compact_without_color_or_pretty_is_a_single_line_object() {
        let packet = Packet::new("pipeline-a", Direction::Outbound, b"hi");
        let out = render_json(&packet, false, false);
        assert!(out.starts_with('{') && out.ends_with('}'));
        assert!(out.contains("\"pipeline\":\"pipeline-a\""));
        assert!(out.contains("\"direction\":\"OUTBOUND\""));
        assert!(out.contains("\"length\":2"));
        assert!(out.contains("\"hex\":\"6869\""));
        assert!(!out.contains('\n'));
    }

    #[test]
    fn render_json_pretty_without_color_is_multi_line() {
        let packet = Packet::new("p", Direction::Outbound, b"hi");
        let out = render_json(&packet, true, false);
        assert!(out.contains('\n'));
    }

    #[test]
    fn render_hexdump_without_color_shows_hex_and_ascii_columns() {
        let packet = Packet::new("p", Direction::Outbound, b"hi");
        let out = render_hexdump(&packet, false);
        assert!(out.contains("[OUTBOUND Payload - 2 bytes]"));
        assert!(out.contains("68 69"));
        assert!(out.contains("| hi"));
    }
}
