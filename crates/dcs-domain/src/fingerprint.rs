//! Content fingerprint — the file-identity value (§10b, decision #33). A photo
//! renamed in place keeps its verdicts and tags because identity is keyed on
//! *content*, not path. This is the pure value type only; computing it from
//! bytes is `dcs-io`'s job (the head+tail+size blake3 hash lives behind the
//! scan), and the type carries no knowledge of how it was derived.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 32-byte content hash identifying one file across renames. Serialized as a
/// lowercase hex string so `project.json` stays human-readable and diffable;
/// stored as raw bytes when it keys the binary cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentFingerprint([u8; 32]);

impl ContentFingerprint {
    /// Wrap a precomputed 32-byte digest.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        ContentFingerprint(bytes)
    }

    /// The raw digest, for keying the binary cache.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex (64 chars). Used for the JSON form and `Display`.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(nibble_to_hex(byte >> 4));
            s.push(nibble_to_hex(byte & 0x0f));
        }
        s
    }

    /// Parse a 64-char lowercase/uppercase hex string. Returns `None` on the
    /// wrong length or a non-hex character.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        let raw = hex.as_bytes();
        for (i, byte) in bytes.iter_mut().enumerate() {
            let hi = hex_to_nibble(raw[i * 2])?;
            let lo = hex_to_nibble(raw[i * 2 + 1])?;
            *byte = (hi << 4) | lo;
        }
        Some(ContentFingerprint(bytes))
    }
}

impl fmt::Display for ContentFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ContentFingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentFingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(HexVisitor)
    }
}

fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + (n - 10)) as char,
    }
}

fn hex_to_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

struct HexVisitor;

impl Visitor<'_> for HexVisitor {
    type Value = ContentFingerprint;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a 64-character hex content fingerprint")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        ContentFingerprint::from_hex(v)
            .ok_or_else(|| de::Error::invalid_value(de::Unexpected::Str(v), &self))
    }
}
