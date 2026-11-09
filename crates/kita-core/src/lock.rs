use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256;
use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::manifest::Scope;
use crate::version::Version;

/// `kita.lock` is the fact layer: exact versions, hashes, and tree digests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
    pub schema: u32,
    pub resolver: String,
    pub platform: String,
    pub manifest: Sha256,
    #[serde(default, rename = "package")]
    pub packages: Vec<LockPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockPackage {
    pub id: PackageId,
    pub version: Version,
    pub source: String,
    pub scope: Scope,
    pub root: bool,
    pub activation: String,
    pub artifact: LockArtifact,
    pub tree: Sha256,
    pub manifest_digest: Sha256,
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockArtifact {
    pub target: String,
    pub sha256: Sha256,
    pub size: u64,
}

pub const LOCK_SCHEMA: u32 = 1;
pub const RESOLVER_VERSION: &str = "1";

pub fn read_optional(path: &Path) -> Result<Option<LockFile>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(KitaError::io(
                "KITA-7001",
                format!("无法读取 {}", path.display()),
                error,
            ))
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        KitaError::new("KITA-7001", format!("kita.lock 不是 UTF-8：{error}"))
    })?;
    let lock: LockFile = toml::from_str(text).map_err(|error| {
        KitaError::new("KITA-7001", format!("kita.lock 解析失败：{error}"))
            .with_hint("删除 kita.lock 后重新运行 `kita sync`")
    })?;
    if lock.schema != LOCK_SCHEMA {
        return Err(KitaError::new(
            "KITA-7001",
            format!("不支持的锁文件 schema：{}", lock.schema),
        ));
    }
    Ok(Some(lock))
}

pub fn write_atomic(path: &Path, lock: &LockFile) -> Result<()> {
    let text = toml::to_string_pretty(lock)
        .map_err(|error| KitaError::new("KITA-7001", format!("序列化锁文件失败：{error}")))?;
    let header = "# 由 Kita 生成；请勿手工编辑。\n";
    crate::util::atomic_write(path, format!("{header}{text}").as_bytes())
}

impl LockFile {
    pub fn package(&self, id: &PackageId) -> Option<&LockPackage> {
        self.packages.iter().find(|package| &package.id == id)
    }
}
