//! Shared identity types: game, content language, profile, timestamps, signatures.

use serde::{Deserialize, Serialize};

/// Which Path of Exile game a profile, catalog, or observation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Game {
    Poe1,
    Poe2,
}

impl Game {
    pub fn as_str(self) -> &'static str {
        match self {
            Game::Poe1 => "poe1",
            Game::Poe2 => "poe2",
        }
    }
}

/// Language of the *game client* being recognized (independent of UI language).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentLanguage {
    English,
    TraditionalChinese,
}

impl ContentLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentLanguage::English => "en",
            ContentLanguage::TraditionalChinese => "zh-TW",
        }
    }
}

/// Recognition profile identity: one game client in one content language.
/// Layout calibration and thresholds are stored per profile (and per resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId {
    pub game: Game,
    pub language: ContentLanguage,
}

impl ProfileId {
    pub const fn new(game: Game, language: ContentLanguage) -> Self {
        Self { game, language }
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.game.as_str(), self.language.as_str())
    }
}

/// Timestamp pair stamped at capture time and carried through to the engine.
/// The monotonic half feeds cross-leg skew gates; the wall half is for storage
/// and display only and must never be used for interval arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureTimestamp {
    pub wall_unix_ms: i64,
    pub mono_ms: u64,
}

/// 64-bit FNV-1a over one byte slice.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hasher = SignatureHasher::new();
    hasher.write_bytes(bytes);
    hasher.finish().0
}

/// Content signature of a recognized order book (need, have, per-row field
/// fingerprints). Equal signatures mean "the same book is still on screen",
/// which lets the monitor dedupe re-displays for the cost of a hash compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BookSignature(pub u64);

/// Incremental FNV-1a hasher used to build [`BookSignature`]s deterministically.
#[derive(Debug, Clone)]
pub struct SignatureHasher(u64);

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl SignatureHasher {
    pub fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    pub fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    pub fn write_str(&mut self, value: &str) {
        self.write_bytes(value.as_bytes());
        // Length terminator so ("ab","c") and ("a","bc") cannot collide.
        self.write_u64(value.len() as u64);
    }

    pub fn finish(&self) -> BookSignature {
        BookSignature(self.0)
    }
}

impl Default for SignatureHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn profile_id_display_is_stable() {
        let profile = ProfileId::new(Game::Poe2, ContentLanguage::TraditionalChinese);
        assert_eq!(profile.to_string(), "poe2-zh-TW");
        assert_eq!(
            ProfileId::new(Game::Poe1, ContentLanguage::English).to_string(),
            "poe1-en"
        );
    }

    #[test]
    fn string_hashing_is_length_delimited() {
        let mut a = SignatureHasher::new();
        a.write_str("ab");
        a.write_str("c");
        let mut b = SignatureHasher::new();
        b.write_str("a");
        b.write_str("bc");
        assert_ne!(a.finish(), b.finish());
    }
}
