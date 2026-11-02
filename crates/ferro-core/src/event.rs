use serde::{Deserialize, Serialize};

/// Something happened in the daemon; broadcast to event subscribers
/// (the menu bar app, `ferro events`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: u64,
    pub kind: String,
    pub message: String,
}

impl Event {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            kind: kind.into(),
            message: message.into(),
        }
    }
}

/// A random URL-safe token (hex), from /dev/urandom. Used for DB
/// passwords and one-time login tokens.
pub fn random_token(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        // Fallback: time-based mixing. Only used if /dev/urandom is
        // somehow unavailable; good enough for local dev tokens.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (now >> (i % 16)) as u8 ^ (i as u8).wrapping_mul(97);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex() {
        let a = random_token(24);
        let b = random_token(24);
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
