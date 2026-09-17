/// Masks matched substrings in a packet's bytes with `*`, in place,
/// preserving length — so hexdump/JSON/base64 all still render a valid,
/// correctly-sized structure around the masked region instead of the real
/// content.
///
/// Runs *after* `PacketFilter` — filtering still sees the real bytes, so
/// `contains = "password"` can gate logging on a secret being present
/// while `redact = ["password"]` keeps that secret out of what actually
/// gets printed.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    patterns: Vec<Vec<u8>>,
}

impl Redactor {
    pub fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_bytes().to_vec())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Overwrite every match of every pattern with `*` bytes. Patterns are
    /// applied in order; a byte already masked by an earlier pattern can
    /// still be scanned (and re-masked, harmlessly) by a later one.
    pub fn apply(&self, bytes: &mut [u8]) {
        for pattern in &self.patterns {
            let plen = pattern.len();
            if plen == 0 || plen > bytes.len() {
                continue;
            }
            let mut i = 0;
            while i + plen <= bytes.len() {
                if &bytes[i..i + plen] == pattern.as_slice() {
                    bytes[i..i + plen].fill(b'*');
                    i += plen;
                } else {
                    i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_every_occurrence() {
        let redactor = Redactor::new(&["secret".to_string()]);
        let mut bytes = b"the secret is secret".to_vec();
        redactor.apply(&mut bytes);
        assert_eq!(&bytes, b"the ****** is ******");
    }

    #[test]
    fn preserves_length_and_leaves_other_bytes_alone() {
        let redactor = Redactor::new(&["X".to_string()]);
        let mut bytes = b"aXbXc".to_vec();
        redactor.apply(&mut bytes);
        assert_eq!(&bytes, b"a*b*c");
    }

    #[test]
    fn empty_patterns_is_a_noop() {
        let redactor = Redactor::default();
        assert!(redactor.is_empty());
        let mut bytes = b"unchanged".to_vec();
        redactor.apply(&mut bytes);
        assert_eq!(&bytes, b"unchanged");
    }
}
