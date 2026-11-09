use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{KitaError, Result};
use crate::util::atomic_write;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Last repository used with `kita sync`. Stored as an absolute path when
    /// possible so later commands do not depend on the working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

pub fn path(kita_root: &Path) -> PathBuf {
    kita_root.join("settings.toml")
}

pub fn load(kita_root: &Path) -> Result<Settings> {
    let path = path(kita_root);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                KitaError::new(
                    "KITA-2001",
                    format!("settings.toml 不是 UTF-8：{error}"),
                )
            })?;
            toml::from_str(text).map_err(|error| {
                KitaError::new("KITA-2001", format!("settings.toml 解析失败：{error}"))
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(KitaError::io(
            "KITA-2001",
            format!("无法读取 {}", path.display()),
            error,
        )),
    }
}

pub fn save(kita_root: &Path, settings: &Settings) -> Result<()> {
    let text = toml::to_string(settings)
        .map_err(|error| KitaError::new("KITA-2001", format!("序列化 settings 失败：{error}")))?;
    atomic_write(&path(kita_root), text.as_bytes())
}
