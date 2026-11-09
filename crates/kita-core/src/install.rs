use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::digest::{sha256_bytes, sha256_file, Sha256};
use crate::error::{KitaError, Result};
use crate::manifest::{load_package, Artifact, InstallSpec, PackageManifest};
use crate::path::RelPath;
use crate::repository::Repository;
use crate::store::Store;
use crate::util::{ensure_dir, unique_suffix};

pub fn cache_path(kita_root: &Path, artifact: &Artifact) -> PathBuf {
    kita_root
        .join("cache")
        .join("downloads")
        .join(artifact.sha256.to_hex())
}

pub fn verify_file(path: &Path, artifact: &Artifact) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|error| {
        KitaError::io(
            "KITA-5002",
            format!("无法读取 artifact {}", path.display()),
            error,
        )
    })?;
    if metadata.len() != artifact.size {
        return Err(KitaError::new(
            "KITA-5002",
            format!(
                "artifact 大小不匹配：{}（期望 {}，实际 {}）",
                path.display(),
                artifact.size,
                metadata.len()
            ),
        ));
    }
    let (digest, _) = sha256_file(path)?;
    if digest != artifact.sha256 {
        return Err(KitaError::new(
            "KITA-5002",
            format!(
                "artifact 哈希不匹配：{}（期望 {}，实际 {}）",
                path.display(),
                artifact.sha256,
                digest
            ),
        )
        .with_hint("仓库中的文件可能已损坏；重新发布并更新清单哈希"));
    }
    Ok(())
}

/// Copy an artifact from the repository into the local cache, verifying it at
/// both ends. Returns the cached path.
pub fn ensure_artifact(
    kita_root: &Path,
    repo: &Repository,
    artifact: &Artifact,
) -> Result<PathBuf> {
    let cached = cache_path(kita_root, artifact);
    if cached.is_file() && verify_file(&cached, artifact).is_ok() {
        return Ok(cached);
    }
    if cached.is_file() {
        let _ = fs::remove_file(&cached);
    }

    let source = repo.artifact_path(artifact)?;
    verify_file(&source, artifact)?;

    ensure_dir(
        cached
            .parent()
            .ok_or_else(|| KitaError::new("KITA-2001", "缓存路径没有父目录"))?,
    )?;
    let temp = cached.with_extension(format!("{}.part", unique_suffix()));
    fs::copy(&source, &temp).map_err(|error| {
        KitaError::io(
            "KITA-4002",
            format!(
                "无法复制 artifact {} 到 {}",
                source.display(),
                temp.display()
            ),
            error,
        )
    })?;
    verify_file(&temp, artifact)?;
    fs::rename(&temp, &cached).map_err(|error| {
        let _ = fs::remove_file(&temp);
        KitaError::io(
            "KITA-4002",
            format!("无法写入缓存 {}", cached.display()),
            error,
        )
    })?;
    Ok(cached)
}

/// Extract a zip archive, refusing traversal and duplicate paths.
pub fn extract_zip(
    archive_path: &Path,
    destination: &Path,
    strip_components: u32,
) -> Result<()> {
    let file = File::open(archive_path).map_err(|error| {
        KitaError::io(
            "KITA-5002",
            format!("无法打开 zip {}", archive_path.display()),
            error,
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        KitaError::new(
            "KITA-5002",
            format!("zip 解析失败 {}：{error}", archive_path.display()),
        )
    })?;

    ensure_dir(destination)?;
    let strip = strip_components as usize;
    let mut seen: BTreeSet<RelPath> = BTreeSet::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            KitaError::new("KITA-5002", format!("无法读取 zip 条目：{error}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            KitaError::new(
                "KITA-5002",
                format!("归档条目包含不安全路径：{}", entry.name()),
            )
        })?;
        let raw = enclosed.to_string_lossy().replace('\\', "/");
        let parts = raw
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.len() <= strip {
            continue;
        }
        let relative = RelPath::parse(&parts[strip..].join("/"))?;
        if !seen.insert(relative.clone()) {
            return Err(KitaError::new(
                "KITA-5002",
                format!("归档包含重复路径：{relative}"),
            ));
        }
        let target = relative.join_under(destination);
        if let Some(parent) = target.parent() {
            ensure_dir(parent)?;
        }
        let mut output = File::create(&target).map_err(|error| {
            KitaError::io(
                "KITA-5002",
                format!("无法写入 {}", target.display()),
                error,
            )
        })?;
        io::copy(&mut entry, &mut output).map_err(|error| {
            KitaError::io(
                "KITA-5002",
                format!("解压 {} 失败", target.display()),
                error,
            )
        })?;
    }

    Ok(())
}

/// Extract a portable package into the CAS, returning its tree digest.
pub fn install_artifact(
    kita_root: &Path,
    store: &Store,
    repo: &Repository,
    manifest: &PackageManifest,
    target: &str,
) -> Result<Sha256> {
    if !matches!(&manifest.install, InstallSpec::Portable { .. }) {
        return Err(KitaError::new(
            "KITA-8001",
            format!(
                "当前构建只支持 portable 包；{} 使用了 native 安装",
                manifest.id
            ),
        ));
    }
    let artifact = manifest.artifact_for(target)?;
    if artifact.format != "zip" {
        return Err(KitaError::new(
            "KITA-5002",
            format!(
                "仓库中的 portable 包只支持 zip，遇到 `{}`",
                artifact.format
            ),
        )
        .with_hint("`.kta` 容器请用 `kita install <文件.kta>` 直接安装"));
    }
    let archive = ensure_artifact(kita_root, repo, artifact)?;

    let temp_root = kita_root.join("tmp");
    ensure_dir(&temp_root)?;
    let staging = temp_root.join(format!("install-{}", unique_suffix()));
    ensure_dir(&staging)?;
    let result = (|| {
        extract_zip(
            &archive,
            &staging,
            artifact.strip_components(manifest.install_strip_components()),
        )?;
        verify_bin_paths(manifest, &staging)?;
        store.import_directory(&staging)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// 校验清单声明的每个可执行文件都存在于解包目录中。
fn verify_bin_paths(manifest: &PackageManifest, directory: &Path) -> Result<()> {
    for bin in &manifest.bins {
        let relative = RelPath::parse(&bin.path)?;
        let file = relative.join_under(directory);
        if !file.is_file() {
            return Err(KitaError::new(
                "KITA-5002",
                format!(
                    "包 {} 缺少清单声明的可执行文件 `{}`（{}）",
                    manifest.id, bin.name, bin.path
                ),
            )
            .with_hint("检查安装产物或 `.kta` 的 payload/ 是否包含 [[bin]] 的 path"));
        }
    }
    Ok(())
}

/// `.kta` 容器根目录中的包清单文件名。
const KTA_MANIFEST: &str = "manifest.toml";
/// `.kta` 容器中包文件的目录名前缀。
const KTA_PAYLOAD: &str = "payload";

/// 从 `.kta` 容器中读取的包信息。
#[derive(Clone, Debug)]
pub struct KtaManifest {
    /// 容器的绝对路径；再次解包时直接复用，避免重复读取与哈希。
    pub path: PathBuf,
    pub manifest: PackageManifest,
    /// 容器内 `manifest.toml` 原始字节的 SHA-256。
    pub manifest_digest: Sha256,
    /// 整个 `.kta` 文件的 SHA-256。
    pub container_digest: Sha256,
    /// 整个 `.kta` 文件的字节数。
    pub container_size: u64,
}

/// 读取 `.kta` 容器根目录的 `manifest.toml`，并计算容器级摘要。
pub fn read_kta_manifest(path: &Path) -> Result<KtaManifest> {
    let (container_digest, container_size) = sha256_file(path)?;
    let file = File::open(path).map_err(|error| {
        KitaError::io(
            "KITA-5002",
            format!("无法打开 .kta 容器 {}", path.display()),
            error,
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        KitaError::new(
            "KITA-5002",
            format!(".kta 容器解析失败 {}：{error}", path.display()),
        )
    })?;
    let mut entry = archive.by_name(KTA_MANIFEST).map_err(|_| {
        KitaError::new(
            "KITA-5002",
            format!(".kta 容器缺少 {KTA_MANIFEST}：{}", path.display()),
        )
        .with_hint("容器根目录必须包含 manifest.toml 与 payload/")
    })?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).map_err(|error| {
        KitaError::io(
            "KITA-5002",
            format!("无法读取 .kta 中的 {KTA_MANIFEST}"),
            error,
        )
    })?;
    drop(entry);

    let manifest = load_package(&bytes)?;
    Ok(KtaManifest {
        path: path.to_path_buf(),
        manifest_digest: sha256_bytes(&bytes),
        manifest,
        container_digest,
        container_size,
    })
}

/// 直接把已读取的 `.kta` 容器中的 `payload/` 解包进 CAS，返回文件树摘要。
///
/// 容器信息由调用方提供，避免整文件重复哈希。
pub fn install_kta_from(
    kita_root: &Path,
    store: &Store,
    kta: &KtaManifest,
    target: &str,
) -> Result<Sha256> {
    if !matches!(&kta.manifest.install, InstallSpec::Portable { .. }) {
        return Err(KitaError::new(
            "KITA-8001",
            format!(
                "当前构建只支持 portable 包；{} 使用了 native 安装",
                kta.manifest.id
            ),
        ));
    }
    kta.manifest.artifact_for(target).map_err(|error| {
        error.with_hint(format!("本地容器 {} 不支持当前平台", kta.path.display()))
    })?;

    let temp_root = kita_root.join("tmp");
    ensure_dir(&temp_root)?;
    let staging = temp_root.join(format!("install-{}", unique_suffix()));
    ensure_dir(&staging)?;
    let result = (|| {
        let file = File::open(&kta.path).map_err(|error| {
            KitaError::io(
                "KITA-5002",
                format!("无法打开 .kta 容器 {}", kta.path.display()),
                error,
            )
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| {
            KitaError::new(
                "KITA-5002",
                format!(".kta 容器解析失败 {}：{error}", kta.path.display()),
            )
        })?;
        extract_zip_prefix(&mut archive, &staging, KTA_PAYLOAD)?;
        verify_bin_paths(&kta.manifest, &staging)?;
        store.import_directory(&staging)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// 只解压归档中位于 `prefix/` 下的条目，并去掉该前缀。
fn extract_zip_prefix(
    archive: &mut zip::ZipArchive<File>,
    destination: &Path,
    prefix: &str,
) -> Result<()> {
    ensure_dir(destination)?;
    let mut seen: BTreeSet<RelPath> = BTreeSet::new();
    let mut found = false;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            KitaError::new("KITA-5002", format!("无法读取 zip 条目：{error}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            KitaError::new(
                "KITA-5002",
                format!("归档条目包含不安全路径：{}", entry.name()),
            )
        })?;
        let raw = enclosed.to_string_lossy().replace('\\', "/");
        let mut parts = raw.split('/').filter(|part| !part.is_empty());
        if parts.next() != Some(prefix) {
            continue;
        }
        let rest = parts.collect::<Vec<_>>().join("/");
        if rest.is_empty() {
            continue;
        }
        let relative = RelPath::parse(&rest)?;
        if !seen.insert(relative.clone()) {
            return Err(KitaError::new(
                "KITA-5002",
                format!("归档包含重复路径：{relative}"),
            ));
        }
        found = true;
        let target = relative.join_under(destination);
        if let Some(parent) = target.parent() {
            ensure_dir(parent)?;
        }
        let mut output = File::create(&target).map_err(|error| {
            KitaError::io(
                "KITA-5002",
                format!("无法写入 {}", target.display()),
                error,
            )
        })?;
        io::copy(&mut entry, &mut output).map_err(|error| {
            KitaError::io(
                "KITA-5002",
                format!("解压 {} 失败", target.display()),
                error,
            )
        })?;
    }

    if !found {
        return Err(KitaError::new(
            "KITA-5002",
            format!("`.kta` 容器缺少 `{prefix}/` 目录"),
        )
        .with_hint("用 zip 工具确认容器结构：manifest.toml + payload/"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cache_path;
    use crate::manifest::Artifact;

    #[test]
    fn cache_is_keyed_by_digest() {
        let artifact = Artifact {
            target: "x86_64-pc-windows-msvc".to_string(),
            url: "blobs/x.zip".to_string(),
            sha256: crate::digest::sha256_bytes(b"x"),
            size: 1,
            format: "zip".to_string(),
            strip_components: None,
            publisher: None,
            require_signature: None,
        };
        let path = cache_path(std::path::Path::new("C:/kita"), &artifact);
        assert!(path.ends_with(artifact.sha256.to_hex()));
    }
}
