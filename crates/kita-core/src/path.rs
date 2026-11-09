use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{KitaError, Result};

/// A validated relative path from an archive or manifest.
///
/// Stored with `/` separators. Rejects absolute paths, traversal, ADS, device
/// names, trailing dots/spaces, invalid Windows characters, and oversized
/// components.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Err(invalid("路径不能为空"));
        }
        if input.contains('\0') {
            return Err(invalid("路径不能包含 NUL"));
        }
        if input.starts_with('/') || input.starts_with('\\') {
            return Err(invalid("不允许绝对路径"));
        }
        if input.len() > 32_767 {
            return Err(invalid("路径超过 32767 个字符"));
        }

        let normalized = input.replace('\\', "/");
        if normalized.len() >= 2 {
            let bytes = normalized.as_bytes();
            if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                return Err(invalid("不允许盘符路径"));
            }
        }

        let mut parts = Vec::new();
        for component in normalized.split('/') {
            if component.is_empty() {
                return Err(invalid("路径包含空目录段"));
            }
            if component == "." || component == ".." {
                return Err(invalid("路径不能包含 `.` 或 `..`"));
            }
            if component.len() > 255 {
                return Err(invalid("单个路径段超过 255 个字符"));
            }
            if component.ends_with(' ') || component.ends_with('.') {
                return Err(invalid("路径段不能以空格或点结尾"));
            }
            if component.contains(':') {
                return Err(invalid("路径不能包含备用数据流（`:`）"));
            }
            if component
                .chars()
                .any(|c| c < ' ' || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
            {
                return Err(invalid("路径包含 Windows 非法字符"));
            }
            if is_reserved_device(component) {
                return Err(invalid("路径包含 Windows 保留设备名"));
            }
            parts.push(component);
        }

        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name();
        let (_, extension) = name.rsplit_once('.')?;
        if extension.is_empty() {
            None
        } else {
            Some(extension)
        }
    }

    pub fn join_under(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        for component in self.0.split('/') {
            path.push(component);
        }
        path
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

fn is_reserved_device(component: &str) -> bool {
    let base = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    if let Some(number) = base.strip_prefix("COM") {
        if number.len() == 1 && number.as_bytes()[0].is_ascii_digit() && number != "0" {
            return true;
        }
    }
    if let Some(number) = base.strip_prefix("LPT") {
        if number.len() == 1 && number.as_bytes()[0].is_ascii_digit() && number != "0" {
            return true;
        }
    }
    false
}

fn invalid(message: &str) -> KitaError {
    KitaError::new("KITA-1005", format!("不安全路径：{message}"))
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RelPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

struct RelPathVisitor;

impl<'de> serde::de::Visitor<'de> for RelPathVisitor {
    type Value = RelPath;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("相对路径字符串")
    }

    fn visit_str<E: serde::de::Error>(
        self,
        value: &str,
    ) -> std::result::Result<Self::Value, E> {
        RelPath::parse(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for RelPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(RelPathVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::RelPath;

    #[test]
    fn normalizes_separators() {
        assert_eq!(RelPath::parse("bin\\rg.exe").unwrap().as_str(), "bin/rg.exe");
    }

    #[test]
    fn rejects_traversal_and_windows_traps() {
        for raw in [
            "../escape",
            "a/../../b",
            "/abs",
            "C:\\Windows\\notepad.exe",
            "CON",
            "dir/NUL.txt",
            "a/",
            "a//b",
            "file.txt:stream",
            "trailing.",
            "trailing ",
        ] {
            assert!(RelPath::parse(raw).is_err(), "{raw} should be rejected");
        }
    }
}
