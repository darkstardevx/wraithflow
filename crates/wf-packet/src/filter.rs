use wf_core::{Direction, Packet};

/// Decides whether a captured packet is worth rendering at all. All
/// conditions must pass (AND, not OR) — an empty filter (the default)
/// matches everything.
#[derive(Debug, Clone, Default)]
pub struct PacketFilter {
    /// Drop anything smaller than this many bytes. Useful for silencing
    /// TCP keepalive noise / empty ACK-only chunks.
    pub min_bytes: usize,
    /// Only match one direction. `None` = both.
    pub direction: Option<Direction>,
    /// Only match packets whose raw bytes contain this substring.
    pub contains: Option<Vec<u8>>,
}

impl PacketFilter {
    pub fn matches(&self, packet: &Packet) -> bool {
        if packet.len() < self.min_bytes {
            return false;
        }
        if let Some(dir) = self.direction {
            if packet.direction != dir {
                return false;
            }
        }
        if let Some(needle) = &self.contains {
            if !contains_bytes(&packet.bytes, needle) {
                return false;
            }
        }
        true
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_bytes_filters_small_packets() {
        let filter = PacketFilter { min_bytes: 10, ..Default::default() };
        let small = Packet::new("p", Direction::Outbound, b"hi");
        let big = Packet::new("p", Direction::Outbound, b"hello world!");
        assert!(!filter.matches(&small));
        assert!(filter.matches(&big));
    }

    #[test]
    fn contains_matches_substring() {
        let filter = PacketFilter {
            contains: Some(b"GET /".to_vec()),
            ..Default::default()
        };
        let hit = Packet::new("p", Direction::Outbound, b"GET /index.html HTTP/1.1");
        let miss = Packet::new("p", Direction::Outbound, b"POST /submit HTTP/1.1");
        assert!(filter.matches(&hit));
        assert!(!filter.matches(&miss));
    }

    #[test]
    fn direction_filters() {
        let filter = PacketFilter {
            direction: Some(Direction::Inbound),
            ..Default::default()
        };
        let inb = Packet::new("p", Direction::Inbound, b"x");
        let outb = Packet::new("p", Direction::Outbound, b"x");
        assert!(filter.matches(&inb));
        assert!(!filter.matches(&outb));
    }
}
