use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{sha256_file, Sha256};
use crate::error::{KitaError, Result};
use crate::path::RelPath;
use crate::util::{atomic_write, ensure_dir, read_json_opt};

pub const TREE_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub path: RelPath,
    pub sha256: Sha256,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeManifest {
    pub schema: u32,
    pub digest: Sha256,
    pub entries: Vec<TreeEntry>,
}

/// Content-addressed store rooted at `<kita root>/store`.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(kita_root: &Path) -> Self {
        Self {
            root: kita_root.join("store"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn object_path(&self, digest: &Sha256) -> PathBuf {
        self.root.join("objects").join(digest.to_hex())
    }

    pub fn tree_path(&self, digest: &Sha256) -> PathBuf {
        self.root.join("trees").join(format!("{}.json", digest.to_hex()))
    }

    pub fn has_object(&self, digest: &Sha256) -> bool {
        self.object_path(digest).is_file()
    }

    pub fn has_tree(&self, digest: &Sha256) -> bool {
        self.tree_path(digest).is_file()
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Sha256> {
        let digest = crate::digest::sha256_bytes(bytes);
        if self.has_object(&digest) {
            return Ok(digest);
        }
        let path = self.object_path(&digest);
        atomic_write(&path, bytes)?;
        Ok(digest)
    }

    /// Copy a file into the CAS, returning its digest and size.
    pub fn put_file(&self, source: &Path) -> Result<(Sha256, u64)> {
        let metadata = fs::metadata(source).map_err(|error| {
            KitaError::io(
                "KITA-5001",
                format!("无法读取文件 {}", source.display()),
                error,
            )
        })?;
        if !metadata.is_file() {
            return Err(KitaError::new(
                "KITA-5001",
                format!("{} 不是普通文件", source.display()),
            ));
        }
        let (digest, _) = sha256_file(source)?;
        if !self.has_object(&digest) {
            let bytes = fs::read(source).map_err(|error| {
                KitaError::io(
                    "KITA-5001",
                    format!("无法读取文件 {}", source.display()),
                    error,
                )
            })?;
            self.put_bytes(&bytes)?;
        }
        Ok((digest, metadata.len()))
    }

    pub fn verify_object(&self, digest: &Sha256, size: u64) -> Result<()> {
        let path = self.object_path(digest);
        let metadata = fs::metadata(&path).map_err(|error| {
            KitaError::io(
                "KITA-5001",
                format!("CAS 中缺少对象 {}", path.display()),
                error,
            )
        })?;
        if metadata.len() != size {
            return Err(KitaError::new(
                "KITA-5001",
                format!(
                    "CAS 对象 {} 大小不匹配（期望 {size}，实际 {}）",
                    digest,
                    metadata.len()
                ),
            ));
        }
        let (actual, _) = sha256_file(&path)?;
        if &actual != digest {
            return Err(KitaError::new(
                "KITA-5001",
                format!("CAS 对象哈希不匹配：{}", path.display()),
            ));
        }
        Ok(())
    }

    /// Write a tree manifest and return its digest.
    pub fn write_tree(&self, entries: &[TreeEntry]) -> Result<Sha256> {
        let mut entries = entries.to_vec();
        entries.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        let digest = tree_digest(&entries);
        let manifest = TreeManifest {
            schema: TREE_SCHEMA,
            digest,
            entries,
        };
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| KitaError::new("KITA-5001", format!("树清单序列化失败：{error}")))?;
        atomic_write(&self.tree_path(&digest), &bytes)?;
        Ok(digest)
    }

    pub fn read_tree(&self, digest: &Sha256) -> Result<TreeManifest> {
        let path = self.tree_path(digest);
        let manifest: TreeManifest = read_json_opt(&path)?.ok_or_else(|| {
            KitaError::new("KITA-5001", format!("缺少树清单 {}", path.display()))
        })?;
        if manifest.schema != TREE_SCHEMA {
            return Err(KitaError::new(
                "KITA-5001",
                format!("不支持的树清单 schema：{}", manifest.schema),
            ));
        }
        if &manifest.digest != digest {
            return Err(KitaError::new(
                "KITA-5001",
                format!("树清单自述摘要与文件名不一致：{}", path.display()),
            ));
        }
        let actual = tree_digest(&manifest.entries);
        if &actual != digest {
            return Err(KitaError::new(
                "KITA-5001",
                format!("树清单内容被篡改：{}", path.display()),
            ));
        }
        Ok(manifest)
    }

    /// Build a tree from an extracted directory: hash every file, place it in
    /// the CAS, and return the tree digest. The directory itself is not kept.
    pub fn import_directory(&self, dir: &Path) -> Result<Sha256> {
        let entries = collect_tree_entries(dir)?;
        for entry in &entries {
            let source = entry.path.join_under(dir);
            let (digest, size) = self.put_file(&source)?;
            if digest != entry.sha256 || size != entry.size {
                return Err(KitaError::new(
                    "KITA-5001",
                    format!("{} 在导入期间发生变化", source.display()),
                ));
            }
        }
        self.write_tree(&entries)
    }

    /// Link (or copy) a tree into `destination`. `mutable` forces copies so the
    /// package can write beside its executable.
    pub fn materialize(&self, entries: &[TreeEntry], destination: &Path, mutable: bool) -> Result<()> {
        ensure_dir(destination)?;
        for entry in entries {
            let source = self.object_path(&entry.sha256);
            if !source.is_file() {
                return Err(KitaError::new(
                    "KITA-5001",
                    format!("CAS 缺少对象 {}", entry.sha256),
                ));
            }
            let target = entry.path.join_under(destination);
            if let Some(parent) = target.parent() {
                ensure_dir(parent)?;
            }
            if target.exists() {
                fs::remove_file(&target).map_err(|error| {
                    KitaError::io(
                        "KITA-2001",
                        format!("无法覆盖 {}", target.display()),
                        error,
                    )
                })?;
            }
            let linked = !mutable && fs::hard_link(&source, &target).is_ok();
            if !linked {
                fs::copy(&source, &target).map_err(|error| {
                    KitaError::io(
                        "KITA-5001",
                        format!("无法复制 {} 到 {}", source.display(), target.display()),
                        error,
                    )
                })?;
            }
        }
        Ok(())
    }
}

/// Canonical tree digest: sorted `sha256 size path` lines.
pub fn tree_digest(entries: &[TreeEntry]) -> Sha256 {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    let mut text = String::new();
    for entry in &sorted {
        text.push_str(&entry.sha256.to_hex());
        text.push(' ');
        text.push_str(&entry.size.to_string());
        text.push(' ');
        text.push_str(entry.path.as_str());
        text.push('\n');
    }
    crate::digest::sha256_bytes(text.as_bytes())
}

/// Walk a directory and produce safe relative entries. Absolute paths, `..`,
/// and unsupported file types are rejected rather than silently skipped.
pub fn collect_tree_entries(dir: &Path) -> Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read_dir = fs::read_dir(&current).map_err(|error| {
            KitaError::io(
                "KITA-5001",
                format!("无法读取目录 {}", current.display()),
                error,
            )
        })?;
        for item in read_dir {
            let item = item.map_err(|error| {
                KitaError::io(
                    "KITA-5001",
                    format!("无法读取目录 {}", current.display()),
                    error,
                )
            })?;
            let path = item.path();
            let metadata = item.metadata().map_err(|error| {
                KitaError::io(
                    "KITA-5001",
                    format!("无法读取 {}", path.display()),
                    error,
                )
            })?;
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(KitaError::new(
                    "KITA-5001",
                    format!("不支持的条目类型（符号链接或设备）：{}", path.display()),
                ));
            }
            let relative = path.strip_prefix(dir).map_err(|_| {
                KitaError::new(
                    "KITA-5001",
                    format!("路径不在解压目录内：{}", path.display()),
                )
            })?;
            let text = relative.to_string_lossy().replace('\\', "/");
            let rel = RelPath::parse(&text).map_err(|error| {
                error.with_hint(format!("检查归档中的路径 {}", path.display()))
            })?;
            let (digest, _) = sha256_file(&path)?;
            entries.push(TreeEntry {
                path: rel,
                sha256: digest,
                size: metadata.len(),
            });
        }
    }
    entries.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    Ok(entries)
}

/// Map of relative path to entry, useful for diagnostics.
pub fn entries_by_path(entries: &[TreeEntry]) -> BTreeMap<&str, &TreeEntry> {
    entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{tree_digest, TreeEntry};
    use crate::digest::sha256_bytes;
    use crate::path::RelPath;

    fn entry(path: &str, data: &[u8]) -> TreeEntry {
        TreeEntry {
            path: RelPath::parse(path).unwrap(),
            sha256: sha256_bytes(data),
            size: data.len() as u64,
        }
    }

    #[test]
    fn tree_digest_is_order_independent() {
        let first = vec![entry("b.txt", b"b"), entry("a.txt", b"a")];
        let second = vec![entry("a.txt", b"a"), entry("b.txt", b"b")];
        assert_eq!(tree_digest(&first), tree_digest(&second));
    }

    #[test]
    fn tree_digest_changes_with_content() {
        let first = vec![entry("a.txt", b"a")];
        let second = vec![entry("a.txt", b"b")];
        assert_ne!(tree_digest(&first), tree_digest(&second));
    }
}
