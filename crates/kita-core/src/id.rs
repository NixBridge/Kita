use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{KitaError, Result};

/// Canonical package identifier: `<namespace>/<name>`.
///
/// Omitted namespaces default to `kita`. Everything is stored lowercase to
/// avoid Windows case-folding ambiguity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId {
    namespace: String,
    name: String,
}

impl PackageId {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("包 ID 不能为空"));
        }
        if !input.is_ascii() {
            return Err(invalid("包 ID 只允许 ASCII 字符"));
        }

        let (namespace, name) = match input.split_once('/') {
            Some((namespace, name)) => {
                if name.contains('/') {
                    return Err(invalid("包 ID 只能包含一个 `/`"));
                }
                (namespace, name)
            }
            None => ("kita", input),
        };

        let namespace = namespace.to_ascii_lowercase();
        let name = name.to_ascii_lowercase();
        validate_segment(&namespace, "namespace")?;
        validate_segment(&name, "name")?;

        Ok(Self { namespace, name })
    }

    pub fn new(namespace: &str, name: &str) -> Result<Self> {
        Self::parse(&format!("{namespace}/{name}"))
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn canonical(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

fn validate_segment(segment: &str, label: &str) -> Result<()> {
    if segment.is_empty() {
        return Err(invalid(&format!("{label} 不能为空")));
    }
    if segment.len() > 64 {
        return Err(invalid(&format!("{label} 超过 64 个字符")));
    }
    let bytes = segment.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(invalid(&format!("{label} 必须以小写字母或数字开头")));
    }
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid(&format!(
            "{label} 只允许小写字母、数字、点、下划线和连字符"
        )));
    }
    Ok(())
}

fn invalid(message: &str) -> KitaError {
    KitaError::new("KITA-1001", format!("无效包 ID：{message}"))
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

impl FromStr for PackageId {
    type Err = KitaError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl Serialize for PackageId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.canonical())
    }
}

struct PackageIdVisitor;

impl<'de> Visitor<'de> for PackageIdVisitor {
    type Value = PackageId;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("包 ID 字符串")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        PackageId::parse(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for PackageId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(PackageIdVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::PackageId;

    #[test]
    fn defaults_namespace() {
        assert_eq!(PackageId::parse("ripgrep").unwrap().canonical(), "kita/ripgrep");
        assert_eq!(
            PackageId::parse("Kita/RipGrep").unwrap().canonical(),
            "kita/ripgrep"
        );
    }

    #[test]
    fn rejects_bad_ids() {
        for raw in ["", "a/b/c", "/ripgrep", "kita/", "kita/rip grep", "kita/rip$grep", "包/名字"] {
            assert!(PackageId::parse(raw).is_err(), "{raw} should be rejected");
        }
    }
}
