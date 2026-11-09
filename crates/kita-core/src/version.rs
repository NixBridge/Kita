use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{KitaError, Result};

/// Windows-style version: 1..=8 numeric segments plus an optional SemVer-like
/// prerelease suffix. Trailing zero segments are removed so `1.2.0.0 == 1.2`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    parts: Vec<u64>,
    pre: Vec<PreId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PreId {
    Num(u64),
    Alpha(String),
}

impl Version {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("版本不能为空"));
        }
        if input.contains('+') {
            return Err(invalid("v1 不支持 build metadata（+ 后缀）"));
        }

        let (base, pre_raw) = match input.find('-') {
            Some(index) => (&input[..index], Some(&input[index + 1..])),
            None => (input, None),
        };

        let mut parts = Vec::new();
        for part in base.split('.') {
            if part.is_empty() {
                return Err(invalid("版本号包含空段"));
            }
            if part.len() > 1 && part.starts_with('0') {
                return Err(invalid("数字段不允许前导零"));
            }
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid("版本号只能包含数字和点"));
            }
            let value = part
                .parse::<u64>()
                .map_err(|_| invalid("数字段超出 u64 范围"))?;
            parts.push(value);
        }

        if parts.len() > 8 {
            return Err(invalid("版本最多允许 8 个数字段"));
        }
        while parts.len() > 1 && parts.last() == Some(&0) {
            parts.pop();
        }

        let pre = match pre_raw {
            Some(raw) => parse_prerelease(raw)?,
            None => Vec::new(),
        };

        Ok(Self { parts, pre })
    }

    pub fn from_parts(parts: Vec<u64>) -> Self {
        let mut parts = if parts.is_empty() { vec![0] } else { parts };
        while parts.len() > 1 && parts.last() == Some(&0) {
            parts.pop();
        }
        Self {
            parts,
            pre: Vec::new(),
        }
    }

    pub fn parts(&self) -> &[u64] {
        &self.parts
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// True when both versions have the same numeric base, ignoring prerelease.
    pub fn same_base(&self, other: &Self) -> bool {
        let len = self.parts.len().max(other.parts.len());
        (0..len).all(|i| self.parts.get(i).copied().unwrap_or(0) == other.parts.get(i).copied().unwrap_or(0))
    }
}

fn invalid(message: &str) -> KitaError {
    KitaError::new("KITA-3001", format!("无效版本：{message}"))
}

fn parse_prerelease(raw: &str) -> Result<Vec<PreId>> {
    if raw.is_empty() {
        return Err(invalid("预发布后缀为空"));
    }
    let mut result = Vec::new();
    for part in raw.split('.') {
        if part.is_empty() {
            return Err(invalid("预发布后缀包含空标识符"));
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(invalid("预发布标识符只能包含字母、数字和连字符"));
        }
        if part.chars().all(|c| c.is_ascii_digit()) {
            if part.len() > 1 && part.starts_with('0') {
                return Err(invalid("预发布数字标识符不允许前导零"));
            }
            let value = part
                .parse::<u64>()
                .map_err(|_| invalid("预发布数字标识符超出 u64 范围"))?;
            result.push(PreId::Num(value));
        } else {
            result.push(PreId::Alpha(part.to_string()));
        }
    }
    Ok(result)
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        for i in 0..len {
            let left = self.parts.get(i).copied().unwrap_or(0);
            let right = other.parts.get(i).copied().unwrap_or(0);
            match left.cmp(&right) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }

        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let len = self.pre.len().min(other.pre.len());
                for i in 0..len {
                    let ordering = match (&self.pre[i], &other.pre[i]) {
                        (PreId::Num(a), PreId::Num(b)) => a.cmp(b),
                        (PreId::Alpha(a), PreId::Alpha(b)) => a.cmp(b),
                        (PreId::Num(_), PreId::Alpha(_)) => Ordering::Less,
                        (PreId::Alpha(_), PreId::Num(_)) => Ordering::Greater,
                    };
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = self
            .parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".");
        write!(f, "{base}")?;
        if !self.pre.is_empty() {
            let pre = self
                .pre
                .iter()
                .map(|part| match part {
                    PreId::Num(value) => value.to_string(),
                    PreId::Alpha(value) => value.clone(),
                })
                .collect::<Vec<_>>()
                .join(".");
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = KitaError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

struct VersionVisitor;

impl<'de> Visitor<'de> for VersionVisitor {
    type Value = Version;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("版本字符串")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Version::parse(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(VersionVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn trims_trailing_zeroes() {
        assert_eq!(Version::parse("1.2.0.0").unwrap().to_string(), "1.2");
        assert_eq!(Version::parse("1.0").unwrap(), Version::parse("1").unwrap());
    }

    #[test]
    fn orders_numeric_segments() {
        assert!(Version::parse("1.10").unwrap() > Version::parse("1.9").unwrap());
        assert!(Version::parse("2").unwrap() > Version::parse("1.99").unwrap());
        assert!(Version::parse("1.2.3").unwrap() > Version::parse("1.2.3-beta.2").unwrap());
        assert!(Version::parse("1.2.3-beta.2").unwrap() > Version::parse("1.2.3-beta.1").unwrap());
    }

    #[test]
    fn roundtrips_display() {
        for raw in ["1", "1.2", "14.1.1", "2024.10", "1.0.0-beta.2", "6.3.9600"] {
            let version = Version::parse(raw).unwrap();
            assert_eq!(Version::parse(&version.to_string()).unwrap(), version);
        }
    }

    #[test]
    fn rejects_invalid_versions() {
        for raw in ["", "01.2", "1..2", "1.2.3+build", "1.2.3-", "1.2.3-beta..1"] {
            assert!(Version::parse(raw).is_err(), "{raw} should be rejected");
        }
    }
}
