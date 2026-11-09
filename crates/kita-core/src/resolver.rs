use std::collections::{BTreeMap, VecDeque};

use crate::digest::Sha256;
use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::lock::LockFile;
use crate::manifest::{
    canonical_digest, Artifact, NormalizedProject, PackageManifest, Scope,
};
use crate::repository::Repository;
use crate::requirement::Requirement;
use crate::version::Version;

/// One concrete package selected for a generation.
#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    pub id: PackageId,
    pub version: Version,
    pub scope: Scope,
    pub source: String,
    /// True when the package was requested directly by `kita.toml`.
    pub root: bool,
    pub manifest: PackageManifest,
    pub manifest_digest: Sha256,
    pub artifact: Artifact,
    /// Command names this instance contributes to the environment. Dependencies
    /// declared with `bins = false` contribute nothing.
    pub bins: Vec<String>,
    pub dependencies: Vec<PackageId>,
}

#[derive(Clone, Debug)]
pub struct Resolution {
    pub packages: BTreeMap<PackageId, ResolvedPackage>,
}

impl Resolution {
    pub fn iter(&self) -> impl Iterator<Item = &ResolvedPackage> {
        self.packages.values()
    }

    /// Ordered `(command, package id)` pairs. Fails when two packages claim the
    /// same command.
    pub fn tools(&self) -> Result<BTreeMap<String, PackageId>> {
        let mut tools = BTreeMap::new();
        for package in self.packages.values() {
            for command in &package.bins {
                if let Some(existing) = tools.insert(command.clone(), package.id.clone()) {
                    return Err(KitaError::new(
                        "KITA-9001",
                        format!(
                            "命令 `{command}` 同时由 {} 和 {} 提供",
                            existing, package.id
                        ),
                    )
                    .with_hint("从 kita.toml 移除其中一个包"));
                }
            }
        }
        Ok(tools)
    }
}

/// Resolve a project against a local repository without consulting a lock file.
///
/// The algorithm is deliberately simple and deterministic: packages are visited
/// in canonical ID order, each visit picks the highest matching non-yanked
/// version, and dependency constraints are checked as they are discovered.
/// There is no backtracking; a genuine conflict is reported with both sides.
pub fn resolve(
    project: &NormalizedProject,
    repo: &Repository,
    target: &str,
) -> Result<Resolution> {
    let mut constraints: BTreeMap<PackageId, Vec<Requirement>> = BTreeMap::new();
    let mut scopes: BTreeMap<PackageId, Scope> = BTreeMap::new();
    let mut sources: BTreeMap<PackageId, String> = BTreeMap::new();
    let mut expose: BTreeMap<PackageId, bool> = BTreeMap::new();

    for (id, request) in &project.packages {
        constraints
            .entry(id.clone())
            .or_default()
            .push(request.requirement.clone());
        scopes.insert(id.clone(), request.scope);
        sources.insert(id.clone(), request.source.clone());
        expose.insert(id.clone(), true);
    }

    let mut selected: BTreeMap<PackageId, ResolvedPackage> = BTreeMap::new();
    let mut queue: VecDeque<PackageId> = constraints.keys().cloned().collect();

    while let Some(id) = queue.pop_front() {
        if selected.contains_key(&id) {
            continue;
        }
        let requirements = constraints
            .get(&id)
            .cloned()
            .unwrap_or_else(|| vec![Requirement::any()]);

        let version = pick_version(repo, &id, &requirements, target)?;
        let (manifest, manifest_digest) = repo.load_manifest(&id, &version)?;
        let artifact = manifest.artifact_for(target)?.clone();

        let scope = scopes.get(&id).copied().unwrap_or(Scope::User);
        let source = sources
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "kita".to_string());
        let root = project.packages.contains_key(&id);
        let bins = if expose.get(&id).copied().unwrap_or(false) {
            manifest.exposed_bins()
        } else {
            Vec::new()
        };

        let mut dependencies = Vec::new();
        for (dep_id, requirement, dep_bins) in manifest.dependency_requirements()? {
            if let Some(parent) = selected.get(&dep_id) {
                if !requirement.contains(&parent.version) {
                    return Err(conflict_error(&dep_id, &parent.version, &requirement, &id));
                }
            }

            constraints
                .entry(dep_id.clone())
                .or_default()
                .push(requirement);
            scopes.entry(dep_id.clone()).or_insert(scope);
            sources
                .entry(dep_id.clone())
                .or_insert_with(|| source.clone());
            if dep_bins {
                expose.insert(dep_id.clone(), true);
            } else {
                expose.entry(dep_id.clone()).or_insert(false);
            }
            dependencies.push(dep_id.clone());

            if !selected.contains_key(&dep_id) {
                queue.push_back(dep_id);
            }
        }

        selected.insert(
            id.clone(),
            ResolvedPackage {
                id,
                version,
                scope,
                source,
                root,
                manifest,
                manifest_digest,
                artifact,
                bins,
                dependencies,
            },
        );
    }

    Ok(Resolution { packages: selected })
}

fn pick_version(
    repo: &Repository,
    id: &PackageId,
    requirements: &[Requirement],
    target: &str,
) -> Result<Version> {
    let index = repo.load_index(id)?;
    let candidate = index
        .versions
        .iter()
        .filter(|entry| !entry.yanked)
        .filter(|entry| target_supported(entry, target))
        .filter(|entry| {
            requirements
                .iter()
                .all(|requirement| requirement.contains(&entry.version))
        })
        .map(|entry| &entry.version)
        .max()
        .cloned();

    candidate.ok_or_else(|| {
        let available = index
            .versions
            .iter()
            .filter(|entry| !entry.yanked)
            .map(|entry| entry.version.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let wanted = requirements
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" 且 ");
        KitaError::new(
            "KITA-3003",
            format!(
                "没有满足 `{wanted}` 的 {id} 版本（{target}；可用：{available}）"
            ),
        )
        .with_hint("放宽 kita.toml 中的版本需求，或向仓库添加匹配的版本")
    })
}

fn target_supported(entry: &crate::repository::IndexVersion, target: &str) -> bool {
    entry.targets.is_empty() || entry.targets.iter().any(|item| item == target)
}

fn conflict_error(
    id: &PackageId,
    selected: &Version,
    requirement: &Requirement,
    from: &PackageId,
) -> KitaError {
    KitaError::new(
        "KITA-3004",
        format!(
            "依赖冲突：{id} 已选择 {selected}，但 {from} 要求 {requirement}"
        ),
    )
    .with_hint("当前解析器不做回溯；请让相关包使用兼容的版本需求")
}

/// Rebuild a resolution from `kita.lock` without re-solving versions.
///
/// Every hash in the lock file is checked against the repository manifest so a
/// tampered lock cannot redirect a download.
pub fn resolve_locked(
    project: &NormalizedProject,
    repo: &Repository,
    target: &str,
    lock: &LockFile,
) -> Result<Resolution> {
    if lock.platform != target {
        return Err(KitaError::new(
            "KITA-7002",
            format!(
                "锁文件平台为 {}，当前平台为 {target}",
                lock.platform
            ),
        )
        .with_hint("删除 kita.lock 后重新运行 `kita sync`"));
    }
    let digest = canonical_digest(project)?;
    if lock.manifest != digest {
        return Err(KitaError::new(
            "KITA-7002",
            "kita.toml 与 kita.lock 不一致",
        )
        .with_hint("运行 `kita sync` 重新解析，或回滚 kita.toml 修改"));
    }

    let mut packages = BTreeMap::new();
    for locked in &lock.packages {
        // 本地 `.kta` 包不来自仓库，锁里只保留其安装事实；
        // 再次安装时由 SyncOptions.locals 显式重新注入。
        if locked.source == "local" {
            continue;
        }
        let (manifest, manifest_digest) = repo.load_manifest(&locked.id, &locked.version)?;
        if manifest_digest != locked.manifest_digest {
            return Err(KitaError::new(
                "KITA-7002",
                format!(
                    "锁文件记录的 {} {} 清单哈希与仓库不一致",
                    locked.id, locked.version
                ),
            ));
        }
        let artifact = manifest.artifact_for(target)?.clone();
        if artifact.sha256 != locked.artifact.sha256 || artifact.size != locked.artifact.size {
            return Err(KitaError::new(
                "KITA-7002",
                format!(
                    "锁文件记录的 {} {} artifact 与仓库 manifest 不一致",
                    locked.id, locked.version
                ),
            ));
        }
        if locked.artifact.target != target {
            return Err(KitaError::new(
                "KITA-7002",
                format!(
                    "锁文件记录的 {} 目标为 {}，当前平台为 {target}",
                    locked.id, locked.artifact.target
                ),
            ));
        }

        let mut dependencies = Vec::new();
        for raw in &locked.dependencies {
            dependencies.push(PackageId::parse(raw)?);
        }

        packages.insert(
            locked.id.clone(),
            ResolvedPackage {
                id: locked.id.clone(),
                version: locked.version.clone(),
                scope: locked.scope,
                source: locked.source.clone(),
                root: locked.root,
                manifest,
                manifest_digest,
                artifact,
                bins: locked.bins.clone(),
                dependencies,
            },
        );
    }

    for (id, request) in &project.packages {
        let locked = packages.get(id).ok_or_else(|| {
            KitaError::new(
                "KITA-7002",
                format!("锁文件缺少根包 {id}"),
            )
            .with_hint("运行 `kita sync` 重建锁文件")
        })?;
        if !request.requirement.contains(&locked.version) {
            return Err(KitaError::new(
                "KITA-7002",
                format!(
                    "锁文件中的 {id} {} 不满足 kita.toml 的 {}",
                    locked.version, request.requirement
                ),
            )
            .with_hint("运行 `kita sync` 重新解析"));
        }
    }

    Ok(Resolution { packages })
}

#[cfg(test)]
mod tests {
    use super::Resolution;
    use crate::id::PackageId;
    use crate::manifest::PackageManifest;

    fn manifest_with_bins(bins: &[&str]) -> PackageManifest {
        let mut text = String::from(
            r#"
schema = 1
id = "kita/demo"
version = "1.0"
summary = "demo"
license = "MIT"
homepage = "https://example.com/demo"

[platform]
arch = ["x86_64"]

[install]
mode = "portable"

[capabilities]
elevation = "never"

[[artifact]]
target = "x86_64-pc-windows-msvc"
url = "blobs/demo.zip"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
size = 1
format = "zip"
"#,
        );
        for name in bins {
            text.push_str(&format!(
                "\n[[bin]]\nname = \"{name}\"\npath = \"{name}.exe\"\n"
            ));
        }
        toml::from_str(&text).expect("fixture manifest parses")
    }

    fn package(id: &str, bins: &[&str]) -> crate::resolver::ResolvedPackage {
        let id = PackageId::parse(id).unwrap();
        let manifest = manifest_with_bins(bins);
        super::ResolvedPackage {
            id: id.clone(),
            version: crate::version::Version::parse("1.0").unwrap(),
            scope: crate::manifest::Scope::User,
            source: "kita".to_string(),
            root: true,
            manifest_digest: crate::digest::sha256_bytes(b"x"),
            artifact: manifest.artifacts[0].clone(),
            manifest,
            bins: bins.iter().map(|item| item.to_string()).collect(),
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn detects_duplicate_commands() {
        let mut resolution = Resolution {
            packages: Default::default(),
        };
        resolution
            .packages
            .insert(PackageId::parse("kita/a").unwrap(), package("kita/a", &["demo"]));
        resolution
            .packages
            .insert(PackageId::parse("kita/b").unwrap(), package("kita/b", &["demo"]));
        assert!(resolution.tools().is_err());
    }
}
