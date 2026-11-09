use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::manifest::{BinKind, Scope};
use crate::util::{read_json_opt, write_json_atomic};
use crate::version::Version;

pub const ACTIVE_SCHEMA: u32 = 1;

/// Runtime truth for shims. Written atomically; never read by the resolver.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveState {
    pub schema: u32,
    pub scope: Scope,
    pub generation: u64,
    /// Relative to the Kita root, e.g. `envs/default/generations/42`.
    pub generation_path: String,
    pub platform: String,
    pub activated_at: String,
    #[serde(default)]
    pub tools: BTreeMap<String, ToolEntry>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEntry {
    pub package: PackageId,
    pub version: Version,
    /// `managed` (relative to generation) or `native` (absolute install dir).
    pub origin: String,
    pub root: String,
    pub path: String,
    #[serde(default)]
    pub kind: BinKind,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl ActiveState {
    pub fn path(kita_root: &Path) -> PathBuf {
        kita_root.join("active.json")
    }

    pub fn load(kita_root: &Path) -> Result<Option<Self>> {
        let path = Self::path(kita_root);
        let state: Option<Self> = read_json_opt(&path)?;
        if let Some(state) = &state {
            if state.schema != ACTIVE_SCHEMA {
                return Err(KitaError::new(
                    "KITA-2002",
                    format!("不支持的 active.json schema：{}", state.schema),
                )
                .with_hint("运行 `kita doctor` 检查本地状态"));
            }
        }
        Ok(state)
    }

    pub fn write(kita_root: &Path, state: &Self) -> Result<()> {
        write_json_atomic(&Self::path(kita_root), state)
    }

    pub fn tool(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    pub fn generation_dir(&self, kita_root: &Path) -> PathBuf {
        kita_root.join(&self.generation_path)
    }
}

#[cfg(test)]
mod tests {
    use super::ActiveState;
    use crate::manifest::Scope;
    use std::collections::BTreeMap;

    #[test]
    fn roundtrips_json() {
        let state = ActiveState {
            schema: 1,
            scope: Scope::User,
            generation: 7,
            generation_path: "envs/default/generations/7".to_string(),
            platform: "x86_64-pc-windows-msvc".to_string(),
            activated_at: "2026-11-09T00:00:00Z".to_string(),
            tools: BTreeMap::new(),
            env: BTreeMap::new(),
        };
        let text = serde_json::to_string(&state).unwrap();
        let parsed: ActiveState = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.generation, 7);
    }
}
