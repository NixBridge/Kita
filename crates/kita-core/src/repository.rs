use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::digest::{sha256_bytes, Sha256};
use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::manifest::{load_package, Artifact, PackageManifest};
use crate::path::RelPath;
use crate::version::Version;

/// Repository metadata. Unknown fields are tolerated so a local checkout of
/// the public repository metadata can be used as a local repository.
#[derive(Clone, Debug, Deserialize)]
pub struct RepositoryMeta {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIndex {
    pub schema: u32,
    pub id: PackageId,
    #[serde(default, rename = "version")]
    pub versions: Vec<IndexVersion>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexVersion {
    pub version: Version,
    pub manifest: String,
    pub manifest_sha256: Sha256,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default)]
    pub yanked_reason: Option<String>,
}

/// A local, static repository rooted at a directory.
#[derive(Clone, Debug)]
pub struct Repository {
    root: PathBuf,
    meta: RepositoryMeta,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        let root = fs::canonicalize(path).map_err(|error| {
            KitaError::io(
                "KITA-4001",
                format!("无法打开仓库 {}", path.display()),
                error,
            )
        })?;
        if !root.is_dir() {
            return Err(KitaError::new(
                "KITA-4001",
                format!("仓库路径不是目录：{}", root.display()),
            ));
        }

        let meta_path = root.join("repository.toml");
        let bytes = fs::read(&meta_path).map_err(|error| {
            KitaError::io(
                "KITA-4001",
                format!("无法读取 {}", meta_path.display()),
                error,
            )
            .with_hint("本地仓库根目录必须包含 repository.toml")
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            KitaError::new("KITA-4001", format!("repository.toml 不是 UTF-8：{error}"))
        })?;
        let meta: RepositoryMeta = toml::from_str(text).map_err(|error| {
            KitaError::new("KITA-4001", format!("repository.toml 解析失败：{error}"))
        })?;

        if meta.schema != 1 {
            return Err(KitaError::new(
                "KITA-4001",
                format!("不支持的仓库 schema：{}", meta.schema),
            ));
        }
        if !is_valid_repo_id(&meta.id) {
            return Err(KitaError::new(
                "KITA-4001",
                format!("非法仓库 ID `{}`", meta.id),
            ));
        }
        if let Some(kind) = &meta.kind {
            if kind != "local" {
                return Err(KitaError::new(
                    "KITA-4001",
                    format!("当前构建只支持本地仓库，仓库类型为 `{kind}`"),
                ));
            }
        }

        Ok(Self { root, meta })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn display_name(&self) -> &str {
        self.meta.name.as_deref().unwrap_or(&self.meta.id)
    }

    pub fn description(&self) -> Option<&str> {
        self.meta.description.as_deref()
    }

    /// Every package that has an `index.toml`, discovered by scanning
    /// `packages/<namespace>/<name>/`. Sorted and deduplicated.
    pub fn package_ids(&self) -> Result<Vec<PackageId>> {
        let packages = self.root.join("packages");
        let namespaces = match fs::read_dir(&packages) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(KitaError::io(
                    "KITA-4001",
                    format!("无法读取 {}", packages.display()),
                    error,
                ))
            }
        };

        let mut ids = Vec::new();
        for namespace in namespaces {
            let namespace = namespace.map_err(|error| {
                KitaError::io(
                    "KITA-4001",
                    format!("无法读取 {}", packages.display()),
                    error,
                )
            })?;
            if !namespace.path().is_dir() {
                continue;
            }
            let namespace_name = namespace.file_name().to_string_lossy().to_string();
            let entries = match fs::read_dir(namespace.path()) {
                Ok(read_dir) => read_dir,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !path.join("index.toml").is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if let Ok(id) = PackageId::new(&namespace_name, &name) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn index_path(&self, id: &PackageId) -> PathBuf {
        self.root
            .join("packages")
            .join(id.namespace())
            .join(id.name())
            .join("index.toml")
    }

    pub fn load_index(&self, id: &PackageId) -> Result<PackageIndex> {
        let path = self.index_path(id);
        let bytes = fs::read(&path).map_err(|error| {
            KitaError::io(
                "KITA-4003",
                format!("仓库中找不到包索引 {}", path.display()),
                error,
            )
            .with_hint(format!("确认包 {id} 存在于仓库 {}", self.root.display()))
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            KitaError::new("KITA-4003", format!("包索引不是 UTF-8：{error}"))
        })?;
        let index: PackageIndex = toml::from_str(text).map_err(|error| {
            KitaError::new("KITA-4003", format!("包索引解析失败：{error}"))
        })?;

        if index.schema != 1 {
            return Err(KitaError::new(
                "KITA-4003",
                format!("不支持的包索引 schema：{}", index.schema),
            ));
        }
        if &index.id != id {
            return Err(KitaError::new(
                "KITA-4003",
                format!("包索引 ID `{}` 与请求的 `{id}` 不一致", index.id),
            ));
        }
        if index.versions.is_empty() {
            return Err(KitaError::new(
                "KITA-4003",
                format!("包 {id} 的索引没有任何版本"),
            ));
        }

        for entry in &index.versions {
            RelPath::parse(&entry.manifest).map_err(|error| {
                error.with_hint(format!("检查 {id} {} 的清单路径", entry.version))
            })?;
            for target in &entry.targets {
                if target != "x86_64-pc-windows-msvc" && target != "aarch64-pc-windows-msvc" {
                    return Err(KitaError::new(
                        "KITA-9002",
                        format!("包索引包含不支持的目标 `{target}`"),
                    ));
                }
            }
        }

        Ok(index)
    }

    pub fn index_entry(&self, id: &PackageId, version: &Version) -> Result<IndexVersion> {
        let index = self.load_index(id)?;
        index
            .versions
            .into_iter()
            .find(|entry| &entry.version == version)
            .ok_or_else(|| {
                KitaError::new(
                    "KITA-4004",
                    format!("仓库中找不到 {id}@{version}"),
                )
            })
    }

    /// Load and verify a manifest, returning the raw manifest digest.
    pub fn load_manifest(
        &self,
        id: &PackageId,
        version: &Version,
    ) -> Result<(PackageManifest, Sha256)> {
        let entry = self.index_entry(id, version)?;
        self.load_manifest_entry(id, &entry)
    }

    pub fn load_manifest_entry(
        &self,
        id: &PackageId,
        entry: &IndexVersion,
    ) -> Result<(PackageManifest, Sha256)> {
        let relative = RelPath::parse(&entry.manifest)?;
        let path = relative.join_under(&self.root);
        ensure_under_root(&self.root, &path)?;

        let bytes = fs::read(&path).map_err(|error| {
            KitaError::io(
                "KITA-4003",
                format!("无法读取包清单 {}", path.display()),
                error,
            )
        })?;
        let digest = sha256_bytes(&bytes);
        if digest != entry.manifest_sha256 {
            return Err(KitaError::new(
                "KITA-5003",
                format!(
                    "包清单哈希不匹配：{id}@{}（期望 {}，实际 {}）",
                    entry.version, entry.manifest_sha256, digest
                ),
            ));
        }

        let manifest = load_package(&bytes)?;
        if &manifest.id != id {
            return Err(KitaError::new(
                "KITA-4003",
                format!("清单 ID `{}` 与索引 ID `{id}` 不一致", manifest.id),
            ));
        }
        if manifest.version != entry.version {
            return Err(KitaError::new(
                "KITA-4003",
                format!(
                    "清单版本 `{}` 与索引版本 `{}` 不一致",
                    manifest.version, entry.version
                ),
            ));
        }

        Ok((manifest, digest))
    }

    pub fn artifact_path(&self, artifact: &Artifact) -> Result<PathBuf> {
        let raw = artifact.url.trim();
        let path = if raw.starts_with("file:") {
            let url = Url::parse(raw).map_err(|error| {
                KitaError::new("KITA-4002", format!("无效的 file:// URL：{error}"))
            })?;
            url.to_file_path().map_err(|_| {
                KitaError::new("KITA-4002", format!("file:// URL 无法转换为本地路径：{raw}"))
            })?
        } else if raw.contains("://") {
            return Err(KitaError::new(
                "KITA-4002",
                "当前构建只支持本地仓库；HTTP 仓库尚未实现",
            )
            .with_hint(format!("将 {} 改为仓库内的相对路径", artifact.target)));
        } else {
            let relative = RelPath::parse(raw).map_err(|error| {
                error.with_hint(format!("检查 artifact `{}` 的 url", artifact.target))
            })?;
            relative.join_under(&self.root)
        };

        if !path.exists() {
            return Err(KitaError::new(
                "KITA-4002",
                format!("artifact 文件不存在：{}", path.display()),
            ));
        }
        let canonical = fs::canonicalize(&path).map_err(|error| {
            KitaError::io(
                "KITA-4002",
                format!("无法解析 artifact 路径 {}", path.display()),
                error,
            )
        })?;
        if raw.starts_with("file:") {
            return Ok(canonical);
        }
        ensure_under_root(&self.root, &canonical)?;
        Ok(canonical)
    }
}

fn ensure_under_root(root: &Path, path: &Path) -> Result<()> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        KitaError::io(
            "KITA-4002",
            format!("无法解析路径 {}", path.display()),
            error,
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(KitaError::new(
            "KITA-4002",
            format!("路径逃逸仓库根目录：{}", canonical.display()),
        ));
    }
    Ok(())
}

fn is_valid_repo_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let bytes = id.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-'))
}
