use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    #[inline]
    pub fn of(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        Self(hex::encode(h.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_input() {
        let a = Fingerprint::of(b"hello world");
        let b = Fingerprint::of(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn different_for_different_input() {
        let a = Fingerprint::of(b"hello world");
        let b = Fingerprint::of(b"hello worlD");
        assert_ne!(a, b);
    }

    #[test]
    fn hex_string_is_64_chars() {
        let f = Fingerprint::of(b"anything");
        assert_eq!(f.as_str().len(), 64);
    }

    #[test]
    fn hex_string_is_lowercase_hex() {
        let f = Fingerprint::of(b"anything");
        assert!(
            f.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got: {}",
            f.as_str()
        );
    }

    #[test]
    fn empty_input_works() {
        let f = Fingerprint::of(b"");
        assert_eq!(f.as_str().len(), 64);
    }
}
