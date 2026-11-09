use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256 as Sha256Hasher};

use crate::error::{KitaError, Result};

/// Lowercase hex SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    pub fn parse_hex(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.len() != 64 || !input.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(KitaError::new(
                "KITA-5001",
                "SHA-256 必须是 64 位十六进制字符串",
            ));
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(input, &mut bytes).map_err(|error| {
            KitaError::new("KITA-5001", format!("无法解析 SHA-256：{error}"))
        })?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.to_hex())
    }
}

impl FromStr for Sha256 {
    type Err = KitaError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse_hex(s)
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

struct Sha256Visitor;

impl<'de> Visitor<'de> for Sha256Visitor {
    type Value = Sha256;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("64 位十六进制 SHA-256")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Sha256::parse_hex(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(Sha256Visitor)
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> Sha256 {
    let mut hasher = Sha256Hasher::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Sha256(out)
}

pub fn sha256_reader<R: std::io::Read>(mut reader: R) -> std::io::Result<(Sha256, u64)> {
    let mut hasher = Sha256Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok((Sha256(out), size))
}

pub fn sha256_file(path: &std::path::Path) -> Result<(Sha256, u64)> {
    let file = std::fs::File::open(path).map_err(|error| {
        KitaError::io(
            "KITA-5001",
            format!("无法读取文件 {}", path.display()),
            error,
        )
    })?;
    sha256_reader(file).map_err(|error| {
        KitaError::io(
            "KITA-5001",
            format!("计算哈希失败 {}", path.display()),
            error,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{sha256_bytes, Sha256};

    #[test]
    fn known_vector() {
        let digest = sha256_bytes(b"abc");
        assert_eq!(
            digest.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(Sha256::parse_hex(&digest.to_hex()).unwrap(), digest);
    }
}
