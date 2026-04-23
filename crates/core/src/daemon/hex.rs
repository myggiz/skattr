// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Hex newtypes for wire-safe byte arrays.
//!
//! Both types display as lowercase hex, parse case-insensitively, and
//! round-trip through serde as hex strings. `Hex32` in particular is
//! what the CLI prints for a pubkey and what the user may prefix-match
//! against via `ContactRepo::lookup_by_prefix`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 16-byte hex newtype (backing `MessageId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex16(pub [u8; 16]);

/// 32-byte hex newtype (backing `PublicKey`, `GroupId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hex32(pub [u8; 32]);

impl From<[u8; 16]> for Hex16 {
    fn from(b: [u8; 16]) -> Self {
        Self(b)
    }
}

impl From<[u8; 32]> for Hex32 {
    fn from(b: [u8; 32]) -> Self {
        Self(b)
    }
}

impl fmt::Display for Hex16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Parse errors for the hex newtypes.
#[derive(Debug, thiserror::Error)]
pub enum HexParseError {
    /// Input string was not the expected length.
    #[error("hex: expected {expected} characters, got {got}")]
    Length {
        /// Number of characters expected.
        expected: usize,
        /// Number of characters actually received.
        got: usize,
    },
    /// Input contained a non-hex character.
    #[error("hex: non-hex character at byte {index}")]
    NonHex {
        /// Byte offset of the offending character.
        index: usize,
    },
}

fn parse_fixed<const N: usize>(s: &str) -> Result<[u8; N], HexParseError> {
    if s.len() != N * 2 {
        return Err(HexParseError::Length {
            expected: N * 2,
            got: s.len(),
        });
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = from_hex_char(chunk[0]).ok_or(HexParseError::NonHex { index: i * 2 })?;
        let lo = from_hex_char(chunk[1]).ok_or(HexParseError::NonHex { index: i * 2 + 1 })?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn from_hex_char(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl FromStr for Hex16 {
    type Err = HexParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_fixed::<16>(s)?))
    }
}

impl FromStr for Hex32 {
    type Err = HexParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_fixed::<32>(s)?))
    }
}

impl Serialize for Hex16 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hex16 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for Hex32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hex32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex16_display_lowercase_roundtrip() {
        let raw = [0xABu8; 16];
        let h = Hex16::from(raw);
        let s = h.to_string();
        assert_eq!(s, "abababababababababababababababab");
        let parsed: Hex16 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex32_display_lowercase_roundtrip() {
        let raw = [0x01u8; 32];
        let h = Hex32::from(raw);
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        let parsed: Hex32 = s.parse().unwrap();
        assert_eq!(parsed.0, raw);
    }

    #[test]
    fn hex16_rejects_wrong_length() {
        let err: Result<Hex16, _> = "aa".parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_rejects_non_hex() {
        let err: Result<Hex32, _> = "zz".repeat(32).parse();
        assert!(err.is_err());
    }

    #[test]
    fn hex32_serde_roundtrip_as_string() {
        let h = Hex32::from([0xCDu8; 32]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&h, &mut buf).unwrap();
        let back: Hex32 = ciborium::de::from_reader(&buf[..]).unwrap();
        assert_eq!(back.0, h.0);
    }
}
