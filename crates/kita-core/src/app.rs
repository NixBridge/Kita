use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::active::{ActiveState, ToolEntry};
use crate::digest::{sha256_bytes, Sha256};
use crate::error::{KitaError, Result};
use crate::generation::{self, BuildPackage, Generation};
use crate::id::PackageId;
use crate::install;
use crate::lock::{
    self, LockArtifact, LockFile, LockPackage, LOCK_SCHEMA, RESOLVER_VERSION,
};
use crate::manifest::{
    add_packages, canonical_digest, load_project, parse_add_spec, remove_packages,
    Artifact, InstallSpec, NormalizedProject, ResolvedRequest, Scope,
};
use crate::plan::{build_plan, Download, Plan};
use crate::repository::Repository;
use crate::requirement::Requirement;
use crate::resolver::{resolve, resolve_locked, Resolution, ResolvedPackage};
use crate::settings;
use crate::shims;
use crate::store::Store;
use crate::util::{self, ensure_dir, is_script_path, StateLock};

const DEFAULT_PROJECT: &str = r#"schema = 1

[environment]
name = "default"
scope = "user"

[packages]
"#;

pub struct Context {
    pub root: PathBuf,
    pub cwd: PathBuf,
}

impl Context {
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|error| {
            KitaError::io("KITA-2001", "无法读取当前目录", error)
        })?;
        let root = root.unwrap_or_else(|| util::default_root());
        let root = if root.is_absolute() {
            root
        } else {
            cwd.join(root)
        };
        Ok(Self { root, cwd })
    }

    pub fn project_path(&self) -> PathBuf {
        self.cwd.join("kita.toml")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.cwd.join("kita.lock")
    }

    fn ensure_root(&self) -> Result<()> {
        ensure_dir(&self.root)?;
        for name in [
            "store",
            "cache",
            "envs",
            "shims",
            "tmp",
            "data",
        ] {
            ensure_dir(&self.root.join(name))?;
        }
        Ok(())
    }

    fn open_repository(&self, override_path: Option<&Path>) -> Result<Repository> {
        let path = match override_path {
            Some(path) => path.to_path_buf(),
            None => match settings::load(&self.root)?.repository {
                Some(raw) => PathBuf::from(raw),
                None => {
                    return Err(KitaError::new(
                        "KITA-4001",
                        "尚未配置包仓库",
                    )
                    .with_hint(
                        "运行 `kita init --repository <本地仓库目录>`，或给命令传入 --repository",
                    ))
                }
            },
        };
        Repository::open(&path)
    }
}

#[derive(Serialize)]
pub struct InitOutcome {
    pub root: String,
    pub project: String,
    pub repository: String,
    pub created_project: bool,
    pub shim_dir: String,
    pub path_hint: String,
}

pub fn init(ctx: &Context, repository: &Path) -> Result<InitOutcome> {
    let repo = Repository::open(repository)?;
    ctx.ensure_root()?;

    let mut current = settings::load(&ctx.root)?;
    current.repository = Some(repo.root().to_string_lossy().to_string());
    settings::save(&ctx.root, &current)?;

    let project = ctx.project_path();
    let created_project = !project.exists();
    if created_project {
        util::atomic_write(&project, DEFAULT_PROJECT.as_bytes())?;
    }
    let shim_dir = shims::shim_dir(&ctx.root);
    ensure_dir(&shim_dir)?;

    Ok(InitOutcome {
        root: ctx.root.to_string_lossy().to_string(),
        project: project.to_string_lossy().to_string(),
        repository: repo.root().to_string_lossy().to_string(),
        created_project,
        shim_dir: shim_dir.to_string_lossy().to_string(),
        path_hint: format!(
            "将 {} 加入用户 PATH 后，包提供的命令即可直接运行；Kita 不会自动修改 PATH。",
            shim_dir.display()
        ),
    })
}

#[derive(Clone, Debug, Default)]
pub struct SyncOptions {
    pub repository: Option<PathBuf>,
    pub locked: bool,
    pub dry_run: bool,
    /// Re-resolve from the manifest even when a lock file exists.
    pub ignore_lock: bool,
    /// 直接安装的 `.kta` 容器路径；这些包不写入 kita.toml。
    pub locals: Vec<PathBuf>,
}

#[derive(Serialize)]
pub struct SyncOutcome {
    pub plan: Plan,
    pub generation: u64,
    pub built: bool,
    pub lock_written: bool,
    pub shim_warnings: Vec<String>,
}

pub fn sync(ctx: &Context, options: &SyncOptions) -> Result<SyncOutcome> {
    let project = load_project(&ctx.project_path())?;
    let repo = ctx.open_repository(options.repository.as_deref())?;
    let platform = util::current_target()?;
    if let Some(pinned) = &project.environment.platform {
        if pinned != &platform {
            return Err(KitaError::new(
                "KITA-9002",
                format!("kita.toml 固定了 {pinned}，当前平台为 {platform}"),
            ));
        }
    }

    let manifest_digest = canonical_digest(&project)?;
    let existing_lock = lock::read_optional(&ctx.lock_path())?;
    let lock = if options.ignore_lock {
        None
    } else {
        existing_lock.clone()
    };
    let mut warnings = Vec::new();

    let mut resolution = match &lock {
        Some(lock) => match resolve_locked(&project, &repo, &platform, lock) {
            Ok(resolution) => resolution,
            Err(error) if options.locked => return Err(error),
            Err(error) => {
                warnings.push(format!(
                    "忽略现有 kita.lock：{}",
                    error.message
                ));
                resolve(&project, &repo, &platform)?
            }
        },
        None => {
            if options.locked {
                return Err(KitaError::new(
                    "KITA-7002",
                    "使用了 --locked，但当前目录没有 kita.lock",
                ));
            }
            resolve(&project, &repo, &platform)?
        }
    };

    // 本地 .kta 容器作为一次性根包加入解析结果；依赖仍从仓库补齐。
    let local_paths = merge_local_packages(
        ctx,
        &project,
        &mut resolution,
        &options.locals,
        &repo,
        &platform,
    )?;

    for package in resolution.iter() {
        if !matches!(&package.manifest.install, InstallSpec::Portable { .. }) {
            return Err(KitaError::new(
                "KITA-8001",
                format!(
                    "{} {} 使用 native 安装；当前构建只支持本地 portable zip",
                    package.id, package.version
                ),
            )
            .with_hint("从 kita.toml 移除此包，或等待 MSI 支持"));
        }
    }

    if !options.dry_run {
        ctx.ensure_root()?;
    }
    let _guard = if options.dry_run {
        None
    } else {
        Some(StateLock::acquire(&ctx.root)?)
    };

    let active = ActiveState::load(&ctx.root)?;
    let active_environment = active
        .as_ref()
        .and_then(|state| environment_from_active(state).ok());
    let current: Option<Generation> = match (&active, active_environment.as_deref()) {
        (Some(state), Some(environment)) if environment == project.environment.name => {
            Some(generation::load(&ctx.root, environment, state.generation)?)
        }
        _ => None,
    };

    let store = Store::new(&ctx.root);
    let mut trees: BTreeMap<PackageId, Sha256> = BTreeMap::new();
    let mut downloads = Vec::new();

    for package in resolution.iter() {
        if let Some(path) = local_paths.get(&package.id) {
            // 本地 .kta 不进入下载缓存；dry-run 时没有文件树，会触发一次重建计划。
            if options.dry_run {
                continue;
            }
            let tree = install::install_kta(&ctx.root, &store, path, &platform)?;
            trees.insert(package.id.clone(), tree);
            continue;
        }

        let cache = install::cache_path(&ctx.root, &package.artifact);
        let cached = cache
            .metadata()
            .map(|metadata| metadata.len() == package.artifact.size)
            .unwrap_or(false);
        if !cached {
            downloads.push(Download {
                id: package.id.canonical(),
                sha256: package.artifact.sha256.to_hex(),
                size: package.artifact.size,
                cached: false,
            });
        }

        let locked_tree = existing_lock
            .as_ref()
            .and_then(|lock| lock.package(&package.id))
            .filter(|locked| locked.version == package.version)
            .map(|locked| locked.tree)
            .filter(|tree| store.has_tree(tree));
        let reusable_tree = locked_tree.or_else(|| {
            current
                .as_ref()
                .and_then(|generation| {
                    generation
                        .packages
                        .iter()
                        .find(|item| item.id == package.id && item.version == package.version)
                        .map(|item| item.tree)
                })
                .filter(|tree| store.has_tree(tree))
        });

        if options.dry_run {
            if let Some(tree) = reusable_tree {
                trees.insert(package.id.clone(), tree);
            }
            continue;
        }

        let tree = match reusable_tree {
            Some(tree) => tree,
            None => {
                let installed = install::install_artifact(
                    &ctx.root,
                    &store,
                    &repo,
                    &package.manifest,
                    &platform,
                )?;
                if let Some(expected) = lock
                    .as_ref()
                    .and_then(|lock| lock.package(&package.id))
                    .map(|locked| locked.tree)
                {
                    if expected != installed {
                        if options.locked {
                            return Err(KitaError::new(
                                "KITA-7002",
                                format!(
                                    "{} {} 的树摘要与锁文件不一致",
                                    package.id, package.version
                                ),
                            ));
                        }
                        warnings.push(format!(
                            "{} {} 的树摘要已变化，锁文件将更新",
                            package.id, package.version
                        ));
                    }
                }
                installed
            }
        };
        trees.insert(package.id.clone(), tree);
    }

    let planned_generation = current
        .as_ref()
        .map(|generation| generation.generation)
        .unwrap_or(0)
        + 1;
    let mut plan = build_plan(
        &platform,
        planned_generation,
        current.as_ref(),
        &resolution,
        &trees,
        downloads,
        manifest_digest,
    );
    plan.warnings.splice(0..0, warnings);
    if !plan.requires_build {
        if let Some(current) = &current {
            plan.generation = current.generation;
        }
    }

    if options.dry_run {
        return Ok(SyncOutcome {
            generation: plan.generation,
            plan,
            built: false,
            lock_written: false,
            shim_warnings: Vec::new(),
        });
    }

    let mut built = false;
    let mut shim_warnings = Vec::new();
    let mut final_generation = current
        .as_ref()
        .map(|generation| generation.generation)
        .unwrap_or(0);

    if plan.requires_build {
        let layers = resolution
            .iter()
            .map(|package| {
                let tree = trees.get(&package.id).copied().ok_or_else(|| {
                    KitaError::new(
                        "KITA-5001",
                        format!("{} 缺少文件树", package.id),
                    )
                })?;
                Ok(BuildPackage {
                    package,
                    tree,
                    activation: "shim",
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let plan_digest = sha256_bytes(
            &serde_json::to_vec(&plan).map_err(|error| {
                KitaError::new("KITA-2001", format!("序列化计划失败：{error}"))
            })?,
        );
        let generation = generation::build(
            &ctx.root,
            &project.environment.name,
            &platform,
            manifest_digest,
            plan_digest,
            &layers,
            &project.env,
        )?;
        shim_warnings.extend(shims::install_shims(&ctx.root, &generation)?);
        let state = generation::active_state(
            &ctx.root,
            &project.environment.name,
            project.environment.scope,
            &platform,
            &generation,
        );
        ActiveState::write(&ctx.root, &state)?;
        final_generation = generation.generation;
        built = true;
    } else if let Some(current) = &current {
        shim_warnings.extend(shims::install_shims(&ctx.root, current)?);
    }

    let lock_file = make_lock(manifest_digest, &resolution, &trees, &platform)?;
    lock::write_atomic(&ctx.lock_path(), &lock_file)?;

    Ok(SyncOutcome {
        plan,
        generation: final_generation,
        built,
        lock_written: true,
        shim_warnings,
    })
}

/// 读取本地 `.kta` 容器并把它们并入解析结果，返回 `包 ID → 容器绝对路径`。
///
/// 本地包被当作一次性的根包：不写入 kita.toml，依赖仍从仓库解析补齐。
fn merge_local_packages(
    ctx: &Context,
    project: &NormalizedProject,
    resolution: &mut Resolution,
    locals: &[PathBuf],
    repo: &Repository,
    platform: &str,
) -> Result<BTreeMap<PackageId, PathBuf>> {
    let mut paths = BTreeMap::new();
    if locals.is_empty() {
        return Ok(paths);
    }

    let mut dependencies: Vec<(PackageId, Requirement, bool)> = Vec::new();
    for raw in locals {
        let path = if raw.is_absolute() {
            raw.clone()
        } else {
            ctx.cwd.join(raw)
        };
        let kta = install::read_kta_manifest(&path)?;
        let manifest = kta.manifest;
        if !matches!(&manifest.install, InstallSpec::Portable { .. }) {
            return Err(KitaError::new(
                "KITA-8001",
                format!(
                    "{} 不是 portable 包；当前构建只支持 portable .kta",
                    manifest.id
                ),
            ));
        }
        manifest.artifact_for(platform).map_err(|error| {
            error.with_hint(format!(
                "本地容器 {} 不支持当前平台",
                path.display()
            ))
        })?;

        let specs = manifest.dependency_requirements()?;
        let dependency_ids = specs
            .iter()
            .map(|(dep_id, _, _)| dep_id.clone())
            .collect::<Vec<_>>();
        dependencies.extend(specs);

        let id = manifest.id.clone();
        let artifact = Artifact {
            target: platform.to_string(),
            url: path.to_string_lossy().to_string(),
            sha256: kta.container_digest,
            size: kta.container_size,
            format: "kta".to_string(),
            strip_components: None,
            publisher: None,
            require_signature: None,
        };
        paths.insert(id.clone(), path);
        resolution.packages.insert(
            id.clone(),
            ResolvedPackage {
                id,
                version: manifest.version.clone(),
                scope: Scope::User,
                source: "local".to_string(),
                root: true,
                bins: manifest.exposed_bins(),
                manifest_digest: kta.manifest_digest,
                manifest,
                artifact,
                dependencies: dependency_ids,
            },
        );
    }

    for (dep_id, requirement, expose) in dependencies {
        if let Some(existing) = resolution.packages.get(&dep_id) {
            if !requirement.contains(&existing.version) {
                return Err(KitaError::new(
                    "KITA-3004",
                    format!(
                        "本地 .kta 要求 {dep_id} {requirement}，但已选择 {}",
                        existing.version
                    ),
                ));
            }
            continue;
        }

        let mut packages = BTreeMap::new();
        packages.insert(
            dep_id.clone(),
            ResolvedRequest {
                requirement,
                scope: Scope::User,
                source: "kita".to_string(),
            },
        );
        let synthetic = NormalizedProject {
            schema: project.schema,
            environment: project.environment.clone(),
            packages,
            env: BTreeMap::new(),
        };
        let sub = resolve(&synthetic, repo, platform)?;
        for (sub_id, package) in sub.packages {
            resolution.packages.entry(sub_id).or_insert(package);
        }
        if !expose {
            if let Some(package) = resolution.packages.get_mut(&dep_id) {
                package.bins.clear();
            }
        }
    }

    Ok(paths)
}

fn make_lock(
    manifest_digest: Sha256,
    resolution: &Resolution,
    trees: &BTreeMap<PackageId, Sha256>,
    platform: &str,
) -> Result<LockFile> {
    let mut packages = Vec::new();
    for package in resolution.iter() {
        let tree = trees.get(&package.id).copied().ok_or_else(|| {
            KitaError::new(
                "KITA-5001",
                format!("{} 缺少文件树，无法写入锁文件", package.id),
            )
        })?;
        packages.push(LockPackage {
            id: package.id.clone(),
            version: package.version.clone(),
            source: package.source.clone(),
            scope: package.scope,
            root: package.root,
            activation: "shim".to_string(),
            artifact: LockArtifact {
                target: package.artifact.target.clone(),
                sha256: package.artifact.sha256,
                size: package.artifact.size,
            },
            tree,
            manifest_digest: package.manifest_digest,
            bins: package.bins.clone(),
            dependencies: package
                .dependencies
                .iter()
                .map(PackageId::canonical)
                .collect(),
        });
    }
    Ok(LockFile {
        schema: LOCK_SCHEMA,
        resolver: RESOLVER_VERSION.to_string(),
        platform: platform.to_string(),
        manifest: manifest_digest,
        packages,
    })
}

pub fn add(
    ctx: &Context,
    specs: &[String],
    repository: Option<&Path>,
) -> Result<SyncOutcome> {
    let parsed = specs
        .iter()
        .map(|spec| parse_add_spec(spec))
        .collect::<Result<Vec<_>>>()?;
    add_packages(&ctx.project_path(), &parsed)?;
    sync(
        ctx,
        &SyncOptions {
            repository: repository.map(Path::to_path_buf),
            locked: false,
            dry_run: false,
            ignore_lock: false,
            locals: Vec::new(),
        },
    )
}

pub fn remove(
    ctx: &Context,
    specs: &[String],
    repository: Option<&Path>,
) -> Result<SyncOutcome> {
    let parsed = specs
        .iter()
        .map(|spec| parse_add_spec(spec).map(|(id, _)| id))
        .collect::<Result<Vec<_>>>()?;
    remove_packages(&ctx.project_path(), &parsed)?;
    sync(
        ctx,
        &SyncOptions {
            repository: repository.map(Path::to_path_buf),
            locked: false,
            dry_run: false,
            ignore_lock: false,
            locals: Vec::new(),
        },
    )
}

#[derive(Serialize)]
pub struct InstalledPackage {
    pub id: String,
    pub version: String,
    pub scope: Scope,
    pub tree: String,
    pub bins: Vec<String>,
}

pub fn list_installed(ctx: &Context) -> Result<Vec<InstalledPackage>> {
    let Some(active) = ActiveState::load(&ctx.root)? else {
        return Ok(Vec::new());
    };
    let environment = environment_from_active(&active)?;
    let generation = generation::load(&ctx.root, &environment, active.generation)?;
    let mut packages = Vec::new();
    for package in &generation.packages {
        let bins = generation
            .tools
            .iter()
            .filter(|(_, tool)| tool.package == package.id)
            .map(|(name, _)| name.clone())
            .collect();
        packages.push(InstalledPackage {
            id: package.id.canonical(),
            version: package.version.to_string(),
            scope: package.scope,
            tree: package.tree.to_hex(),
            bins,
        });
    }
    Ok(packages)
}

#[derive(Serialize)]
pub struct GenerationSummary {
    pub generation: u64,
    pub created_at: String,
    pub platform: String,
    pub packages: usize,
    pub current: bool,
}

pub fn generations(ctx: &Context) -> Result<Vec<GenerationSummary>> {
    let active = ActiveState::load(&ctx.root)?;
    let environment = match &active {
        Some(state) => environment_from_active(state)?,
        None => "default".to_string(),
    };
    let current = active.as_ref().map(|state| state.generation);
    let mut summaries = Vec::new();
    for number in generation::list(&ctx.root, &environment)? {
        let Ok(generation) = generation::load(&ctx.root, &environment, number) else {
            continue;
        };
        summaries.push(GenerationSummary {
            generation: generation.generation,
            created_at: generation.created_at,
            platform: generation.platform,
            packages: generation.packages.len(),
            current: Some(generation.generation) == current,
        });
    }
    Ok(summaries)
}

#[derive(Serialize)]
pub struct RollbackOutcome {
    pub from: u64,
    pub to: u64,
    pub tools: usize,
}

pub fn rollback(ctx: &Context) -> Result<RollbackOutcome> {
    let active = ActiveState::load(&ctx.root)?.ok_or_else(|| {
        KitaError::new("KITA-2002", "当前没有活动环境").with_hint("先运行 `kita sync`")
    })?;
    let environment = environment_from_active(&active)?;
    let target = generation::previous(&ctx.root, &environment, active.generation)?
        .ok_or_else(|| {
            KitaError::new(
                "KITA-2002",
                format!("没有可回滚的 generation（当前 {}）", active.generation),
            )
        })?;
    let generation = generation::load(&ctx.root, &environment, target)?;
    shims::install_shims(&ctx.root, &generation)?;
    let state = generation::active_state(
        &ctx.root,
        &environment,
        active.scope,
        &generation.platform,
        &generation,
    );
    ActiveState::write(&ctx.root, &state)?;
    Ok(RollbackOutcome {
        from: active.generation,
        to: target,
        tools: generation.tools.len(),
    })
}

pub fn env_vars(ctx: &Context) -> Result<BTreeMap<String, String>> {
    Ok(ActiveState::load(&ctx.root)?
        .map(|state| state.env)
        .unwrap_or_default())
}

#[derive(Serialize)]
pub struct VerifyReport {
    pub generation: Option<u64>,
    pub objects_checked: usize,
    pub failures: Vec<String>,
}

pub fn verify(ctx: &Context) -> Result<VerifyReport> {
    let Some(active) = ActiveState::load(&ctx.root)? else {
        return Ok(VerifyReport {
            generation: None,
            objects_checked: 0,
            failures: Vec::new(),
        });
    };
    let environment = environment_from_active(&active)?;
    let generation = generation::load(&ctx.root, &environment, active.generation)?;
    let store = Store::new(&ctx.root);
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for package in &generation.packages {
        let tree = match store.read_tree(&package.tree) {
            Ok(tree) => tree,
            Err(error) => {
                failures.push(format!("{}: {}", package.id, error.message));
                continue;
            }
        };
        for entry in &tree.entries {
            checked += 1;
            if let Err(error) = store.verify_object(&entry.sha256, entry.size) {
                failures.push(format!("{} {}: {}", package.id, entry.path, error.message));
            }
        }
    }
    Ok(VerifyReport {
        generation: Some(generation.generation),
        objects_checked: checked,
        failures,
    })
}

pub fn run_tool(ctx: &Context, name: &str, args: &[String]) -> Result<i32> {
    let active = ActiveState::load(&ctx.root)?.ok_or_else(|| {
        KitaError::new("KITA-9004", "当前没有活动环境").with_hint("先运行 `kita sync`")
    })?;
    let tool = active.tool(name).ok_or_else(|| {
        KitaError::new("KITA-9004", format!("未知命令 `{name}`"))
            .with_hint("运行 `kita list` 查看当前可用命令")
    })?;
    let target = resolve_tool_target(&ctx.root, &active, tool)?;

    let mut command = if is_script_path(&target) {
        let mut command = Command::new("cmd.exe");
        command.arg("/d").arg("/c").arg(&target);
        command
    } else {
        Command::new(&target)
    };
    command.args(args);
    for (key, value) in &active.env {
        command.env(key, value);
    }
    for (key, value) in &tool.env {
        command.env(key, value);
    }

    let status = command.status().map_err(|error| {
        KitaError::io(
            "KITA-9004",
            format!("无法启动 {}", target.display()),
            error,
        )
    })?;
    Ok(status.code().unwrap_or(1))
}

fn resolve_tool_target(
    kita_root: &Path,
    active: &ActiveState,
    tool: &ToolEntry,
) -> Result<PathBuf> {
    if tool.origin != "managed" {
        return Err(KitaError::new(
            "KITA-9004",
            "当前构建不支持 native 工具",
        ));
    }
    let generation_dir = active.generation_dir(kita_root);
    let canonical_generation = fs::canonicalize(&generation_dir).map_err(|error| {
        KitaError::io(
            "KITA-2002",
            format!("无法解析 generation 目录 {}", generation_dir.display()),
            error,
        )
    })?;
    let root = crate::path::RelPath::parse(&tool.root)?;
    let path = crate::path::RelPath::parse(&tool.path)?;
    let target = path.join_under(&root.join_under(&generation_dir));
    if !target.exists() {
        return Err(KitaError::new(
            "KITA-9004",
            format!("工具目标不存在：{}", target.display()),
        )
        .with_hint("运行 `kita sync` 重新生成文件树"));
    }
    let canonical = fs::canonicalize(&target).map_err(|error| {
        KitaError::io(
            "KITA-9004",
            format!("无法解析工具路径 {}", target.display()),
            error,
        )
    })?;
    if !canonical.starts_with(&canonical_generation) {
        return Err(KitaError::new(
            "KITA-9004",
            format!("工具路径逃逸 generation：{}", canonical.display()),
        ));
    }
    Ok(canonical)
}

pub fn environment_from_active(state: &ActiveState) -> Result<String> {
    let parts = state.generation_path.split('/').collect::<Vec<_>>();
    if parts.len() == 4 && parts[0] == "envs" && parts[2] == "generations" {
        Ok(parts[1].to_string())
    } else {
        Err(KitaError::new(
            "KITA-2002",
            format!("active.json 的 generation_path 非法：{}", state.generation_path),
        )
        .with_hint("运行 `kita doctor` 或删除 active.json 后重新 sync"))
    }
}

pub fn install(
    ctx: &Context,
    specs: &[String],
    repository: Option<&Path>,
) -> Result<SyncOutcome> {
    if specs.is_empty() {
        sync(
            ctx,
            &SyncOptions {
                repository: repository.map(Path::to_path_buf),
                locked: false,
                dry_run: false,
                ignore_lock: false,
                locals: Vec::new(),
            },
        )
    } else {
        add(ctx, specs, repository)
    }
}

pub fn update(
    ctx: &Context,
    repository: Option<&Path>,
    dry_run: bool,
) -> Result<SyncOutcome> {
    sync(
        ctx,
        &SyncOptions {
            repository: repository.map(Path::to_path_buf),
            locked: false,
            dry_run,
            ignore_lock: true,
            locals: Vec::new(),
        },
    )
}

#[derive(Serialize)]
pub struct LockPackageSummary {
    pub id: String,
    pub version: String,
    pub tree: String,
}

#[derive(Serialize)]
pub struct LockOutcome {
    pub path: String,
    pub platform: String,
    pub manifest: String,
    pub packages: Vec<LockPackageSummary>,
    pub written: bool,
    pub problems: Vec<String>,
}

pub fn lock(ctx: &Context, check: bool, repository: Option<&Path>) -> Result<LockOutcome> {
    let project = load_project(&ctx.project_path())?;
    let repo = ctx.open_repository(repository)?;
    let platform = util::current_target()?;
    let manifest_digest = canonical_digest(&project)?;
    let existing = lock::read_optional(&ctx.lock_path())?;

    if check {
        let lock_file = existing.ok_or_else(|| {
            KitaError::new("KITA-7002", "当前目录没有 kita.lock")
                .with_hint("运行 `kita lock` 生成锁文件")
        })?;
        let mut problems = Vec::new();
        if let Err(error) = resolve_locked(&project, &repo, &platform, &lock_file) {
            problems.push(error.message.clone());
        }
        let store = Store::new(&ctx.root);
        let mut packages = Vec::new();
        for package in &lock_file.packages {
            if !store.has_tree(&package.tree) {
                problems.push(format!(
                    "{} {} 的文件树不在 CAS 中",
                    package.id, package.version
                ));
            }
            packages.push(LockPackageSummary {
                id: package.id.canonical(),
                version: package.version.to_string(),
                tree: package.tree.to_hex(),
            });
        }
        return Ok(LockOutcome {
            path: ctx.lock_path().to_string_lossy().to_string(),
            platform: lock_file.platform.clone(),
            manifest: lock_file.manifest.to_hex(),
            packages,
            written: false,
            problems,
        });
    }

    let resolution = match &existing {
        Some(lock_file) => match resolve_locked(&project, &repo, &platform, lock_file) {
            Ok(resolution) => resolution,
            Err(_) => resolve(&project, &repo, &platform)?,
        },
        None => resolve(&project, &repo, &platform)?,
    };

    for package in resolution.iter() {
        if !matches!(&package.manifest.install, InstallSpec::Portable { .. }) {
            return Err(KitaError::new(
                "KITA-8001",
                format!(
                    "{} {} 使用 native 安装；当前构建只支持本地 portable zip",
                    package.id, package.version
                ),
            )
            .with_hint("从 kita.toml 移除此包，或等待 MSI 支持"));
        }
    }

    ctx.ensure_root()?;
    let _guard = StateLock::acquire(&ctx.root)?;
    let store = Store::new(&ctx.root);
    let mut trees: BTreeMap<PackageId, Sha256> = BTreeMap::new();
    let mut packages = Vec::new();

    for package in resolution.iter() {
        let reused = existing
            .as_ref()
            .and_then(|lock_file| lock_file.package(&package.id))
            .filter(|locked| locked.version == package.version)
            .map(|locked| locked.tree)
            .filter(|tree| store.has_tree(tree));
        let tree = match reused {
            Some(tree) => tree,
            None => install::install_artifact(
                &ctx.root,
                &store,
                &repo,
                &package.manifest,
                &platform,
            )?,
        };
        trees.insert(package.id.clone(), tree);
        packages.push(LockPackageSummary {
            id: package.id.canonical(),
            version: package.version.to_string(),
            tree: tree.to_hex(),
        });
    }

    let lock_file = make_lock(manifest_digest, &resolution, &trees, &platform)?;
    lock::write_atomic(&ctx.lock_path(), &lock_file)?;

    Ok(LockOutcome {
        path: ctx.lock_path().to_string_lossy().to_string(),
        platform,
        manifest: manifest_digest.to_hex(),
        packages,
        written: true,
        problems: Vec::new(),
    })
}

#[derive(Serialize)]
pub struct SearchHit {
    pub id: String,
    pub latest: String,
    pub versions: usize,
    pub summary: Option<String>,
    pub yanked: bool,
}

pub fn search(
    ctx: &Context,
    query: &str,
    repository: Option<&Path>,
) -> Result<Vec<SearchHit>> {
    let repo = ctx.open_repository(repository)?;
    let needle = query.trim().to_ascii_lowercase();
    let mut hits = Vec::new();
    for id in repo.package_ids()? {
        let index = match repo.load_index(&id) {
            Ok(index) => index,
            Err(_) => continue,
        };
        let latest = index
            .versions
            .iter()
            .filter(|entry| !entry.yanked)
            .max_by(|left, right| left.version.cmp(&right.version))
            .or_else(|| {
                index
                    .versions
                    .iter()
                    .max_by(|left, right| left.version.cmp(&right.version))
            });
        let Some(latest) = latest else {
            continue;
        };
        let summary = repo
            .load_manifest(&id, &latest.version)
            .ok()
            .map(|(manifest, _)| manifest.summary);
        let matched = needle.is_empty()
            || id.canonical().contains(&needle)
            || summary
                .as_deref()
                .map(|value| value.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false);
        if !matched {
            continue;
        }
        hits.push(SearchHit {
            id: id.canonical(),
            latest: latest.version.to_string(),
            versions: index.versions.len(),
            summary,
            yanked: latest.yanked,
        });
    }
    Ok(hits)
}

#[derive(Serialize)]
pub struct PackageVersionInfo {
    pub version: String,
    pub manifest: String,
    pub targets: Vec<String>,
    pub yanked: bool,
    pub yanked_reason: Option<String>,
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct PackageInfo {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    pub license: String,
    pub homepage: String,
    pub upstream: Option<String>,
    pub install: String,
    pub bins: Vec<String>,
    pub dependencies: Vec<String>,
    pub versions: Vec<PackageVersionInfo>,
}

pub fn info(ctx: &Context, raw: &str, repository: Option<&Path>) -> Result<PackageInfo> {
    let id = PackageId::parse(raw)?;
    let repo = ctx.open_repository(repository)?;
    let index = repo.load_index(&id)?;
    let latest = index
        .versions
        .iter()
        .filter(|entry| !entry.yanked)
        .max_by(|left, right| left.version.cmp(&right.version))
        .or_else(|| {
            index
                .versions
                .iter()
                .max_by(|left, right| left.version.cmp(&right.version))
        })
        .ok_or_else(|| KitaError::new("KITA-4004", format!("仓库中找不到 {id}")))?;

    let manifest = repo
        .load_manifest(&id, &latest.version)
        .ok()
        .map(|(manifest, _)| manifest);

    let (summary, description, license, homepage, upstream, install, bins, dependencies) =
        match &manifest {
            Some(manifest) => (
                manifest.summary.clone(),
                manifest.description.clone(),
                manifest.license.clone(),
                manifest.homepage.clone(),
                manifest.upstream.clone(),
                match &manifest.install {
                    InstallSpec::Portable { .. } => "portable".to_string(),
                    InstallSpec::Native { .. } => "native".to_string(),
                },
                manifest.exposed_bins(),
                manifest.dependencies.keys().cloned().collect::<Vec<_>>(),
            ),
            None => (
                String::new(),
                None,
                String::new(),
                String::new(),
                None,
                "unknown".to_string(),
                Vec::new(),
                Vec::new(),
            ),
        };

    let versions = index
        .versions
        .iter()
        .map(|entry| PackageVersionInfo {
            version: entry.version.to_string(),
            manifest: entry.manifest.clone(),
            targets: entry.targets.clone(),
            yanked: entry.yanked,
            yanked_reason: entry.yanked_reason.clone(),
            dependencies: entry.dependencies.clone(),
        })
        .collect();

    Ok(PackageInfo {
        id: id.canonical(),
        summary,
        description,
        license,
        homepage,
        upstream,
        install,
        bins,
        dependencies,
        versions,
    })
}

#[derive(Serialize)]
pub struct WhyOutcome {
    pub package: String,
    pub version: String,
    pub root: bool,
    pub commands: Vec<String>,
    pub chains: Vec<Vec<String>>,
}

pub fn why(ctx: &Context, target: &str) -> Result<WhyOutcome> {
    let active = ActiveState::load(&ctx.root)?.ok_or_else(|| {
        KitaError::new("KITA-9004", "当前没有活动环境").with_hint("先运行 `kita sync`")
    })?;
    let environment = environment_from_active(&active)?;
    let generation_value = generation::load(&ctx.root, &environment, active.generation)?;

    let id = if let Some(tool) = active.tool(target) {
        tool.package.clone()
    } else {
        PackageId::parse(target)?
    };

    let package = generation_value
        .packages
        .iter()
        .find(|package| package.id == id)
        .ok_or_else(|| {
            KitaError::new("KITA-4004", format!("{id} 不在当前 generation 中"))
                .with_hint("运行 `kita list` 查看已安装的包")
        })?;

    let commands = generation_value
        .tools
        .iter()
        .filter(|(_, tool)| tool.package == id)
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();

    let lock_file = lock::read_optional(&ctx.lock_path())?;
    let root = lock_file
        .as_ref()
        .and_then(|lock_file| lock_file.package(&id))
        .map(|package| package.root)
        .unwrap_or(false);
    let chains = lock_file
        .as_ref()
        .map(|lock_file| dependency_chains(lock_file, &id))
        .unwrap_or_default();

    Ok(WhyOutcome {
        package: id.canonical(),
        version: package.version.to_string(),
        root,
        commands,
        chains,
    })
}

fn dependency_chains(lock_file: &LockFile, target: &PackageId) -> Vec<Vec<String>> {
    let mut dependencies: BTreeMap<PackageId, Vec<PackageId>> = BTreeMap::new();
    let mut roots = Vec::new();
    for package in &lock_file.packages {
        let children = package
            .dependencies
            .iter()
            .filter_map(|raw| PackageId::parse(raw).ok())
            .collect::<Vec<_>>();
        dependencies.insert(package.id.clone(), children);
        if package.root {
            roots.push(package.id.clone());
        }
    }

    let mut chains = Vec::new();
    for root in roots {
        let mut stack = vec![(root.clone(), vec![root])];
        while let Some((current, path)) = stack.pop() {
            if &current == target {
                chains.push(path.iter().map(PackageId::canonical).collect::<Vec<_>>());
                if chains.len() >= 8 {
                    chains.sort();
                    chains.dedup();
                    return chains;
                }
                continue;
            }
            if path.len() > 32 {
                continue;
            }
            if let Some(children) = dependencies.get(&current) {
                for child in children {
                    if path.contains(child) {
                        continue;
                    }
                    let mut next = path.clone();
                    next.push(child.clone());
                    stack.push((child.clone(), next));
                }
            }
        }
    }
    chains.sort();
    chains.dedup();
    chains
}

#[derive(Serialize)]
pub struct DiffEntry {
    pub id: String,
    pub manifest: Option<String>,
    pub lock: Option<String>,
    pub active: Option<String>,
    pub issue: String,
}

#[derive(Serialize)]
pub struct DiffOutcome {
    pub manifest_changed: bool,
    pub lock_present: bool,
    pub active_generation: Option<u64>,
    pub entries: Vec<DiffEntry>,
}

pub fn diff(ctx: &Context) -> Result<DiffOutcome> {
    let project = load_project(&ctx.project_path())?;
    let manifest_digest = canonical_digest(&project)?;
    let lock_file = lock::read_optional(&ctx.lock_path())?;

    let active = ActiveState::load(&ctx.root)?;
    let mut active_packages: BTreeMap<PackageId, String> = BTreeMap::new();
    let mut active_generation = None;
    if let Some(state) = &active {
        let environment = environment_from_active(state)?;
        let generation_value = generation::load(&ctx.root, &environment, state.generation)?;
        for package in &generation_value.packages {
            active_packages.insert(package.id.clone(), package.version.to_string());
        }
        active_generation = Some(state.generation);
    }

    let mut locked_packages: BTreeMap<PackageId, String> = BTreeMap::new();
    if let Some(lock_file) = &lock_file {
        for package in &lock_file.packages {
            locked_packages.insert(package.id.clone(), package.version.to_string());
        }
    }

    let mut ids: BTreeSet<PackageId> = project.packages.keys().cloned().collect();
    ids.extend(locked_packages.keys().cloned());
    ids.extend(active_packages.keys().cloned());

    let mut entries = Vec::new();
    for id in ids {
        let requirement = project
            .packages
            .get(&id)
            .map(|request| request.requirement.clone());
        let locked = locked_packages.get(&id).cloned();
        let active_version = active_packages.get(&id).cloned();

        let mut issue = if requirement.is_some() && locked.is_none() {
            "未写入锁文件"
        } else if requirement.is_none() && locked.is_some() {
            "锁文件中的多余包"
        } else if locked.is_some() && active_version.is_none() {
            "尚未激活"
        } else if let (Some(locked), Some(active_version)) = (&locked, &active_version) {
            if locked != active_version {
                "锁文件与活动版本不一致"
            } else {
                "一致"
            }
        } else {
            "一致"
        };

        if issue == "一致" {
            if let (Some(requirement), Some(locked)) = (&requirement, &locked) {
                if let Ok(version) = crate::version::Version::parse(locked) {
                    if !requirement.contains(&version) {
                        issue = "活动版本不满足清单需求";
                    }
                }
            }
        }

        entries.push(DiffEntry {
            id: id.canonical(),
            manifest: requirement.map(|requirement| requirement.to_string()),
            lock: locked,
            active: active_version,
            issue: issue.to_string(),
        });
    }

    let manifest_changed = lock_file
        .as_ref()
        .map(|lock_file| lock_file.manifest != manifest_digest)
        .unwrap_or(true);

    Ok(DiffOutcome {
        manifest_changed,
        lock_present: lock_file.is_some(),
        active_generation,
        entries,
    })
}

pub fn exec(ctx: &Context, program: &str, args: &[String]) -> Result<i32> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(active) = ActiveState::load(&ctx.root)? {
        for (key, value) in &active.env {
            command.env(key, value);
        }
        prepend_shims(&mut command, &ctx.root);
    }
    let status = command.status().map_err(|error| {
        KitaError::io("KITA-9004", format!("无法启动 `{program}`"), error)
    })?;
    Ok(status.code().unwrap_or(1))
}

pub fn shell(ctx: &Context, powershell: bool) -> Result<i32> {
    let mut command = if powershell {
        let mut command = Command::new("powershell.exe");
        command.arg("-NoLogo");
        command
    } else {
        Command::new("cmd.exe")
    };
    if let Some(active) = ActiveState::load(&ctx.root)? {
        for (key, value) in &active.env {
            command.env(key, value);
        }
        prepend_shims(&mut command, &ctx.root);
    }
    let status = command.status().map_err(|error| {
        KitaError::io("KITA-9004", "无法启动 shell", error)
    })?;
    Ok(status.code().unwrap_or(1))
}

fn prepend_shims(command: &mut Command, kita_root: &Path) {
    let mut paths = vec![shims::shim_dir(kita_root)];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env("PATH", joined);
    }
}

#[derive(Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub fixed: Vec<String>,
}

fn push_check(
    checks: &mut Vec<DoctorCheck>,
    name: &str,
    status: &str,
    message: impl Into<String>,
) {
    checks.push(DoctorCheck {
        name: name.to_string(),
        status: status.to_string(),
        message: message.into(),
    });
}

pub fn doctor(ctx: &Context, fix: bool) -> Result<DoctorReport> {
    let mut checks = Vec::new();
    let mut fixed = Vec::new();

    if fix && !ctx.root.is_dir() {
        if ctx.ensure_root().is_ok() {
            fixed.push(format!("创建状态根目录 {}", ctx.root.display()));
            push_check(&mut checks, "root", "ok", "状态根目录已创建");
        } else {
            push_check(&mut checks, "root", "fail", "无法创建状态根目录");
        }
    } else if ctx.root.is_dir() {
        push_check(&mut checks, "root", "ok", ctx.root.to_string_lossy().to_string());
    } else {
        push_check(&mut checks, "root", "fail", "状态根目录不存在");
    }

    match settings::load(&ctx.root) {
        Ok(settings) => match settings.repository {
            Some(raw) => match Repository::open(Path::new(&raw)) {
                Ok(repository) => push_check(
                    &mut checks,
                    "repository",
                    "ok",
                    repository.root().to_string_lossy().to_string(),
                ),
                Err(error) => push_check(&mut checks, "repository", "fail", error.message),
            },
            None => push_check(
                &mut checks,
                "repository",
                "warn",
                "尚未配置包仓库；运行 `kita init --repository <目录>`",
            ),
        },
        Err(error) => push_check(&mut checks, "settings", "fail", error.message),
    }

    let active = ActiveState::load(&ctx.root)?;
    match &active {
        None => push_check(&mut checks, "active", "warn", "当前没有活动环境"),
        Some(state) => {
            push_check(
                &mut checks,
                "active",
                "ok",
                format!("generation {}（{}）", state.generation, state.generation_path),
            );
            let environment = environment_from_active(state)?;
            let directory = state.generation_dir(&ctx.root);
            let ready_path = directory.join(generation::READY_FILE);
            if ready_path.is_file() {
                push_check(&mut checks, "ready", "ok", "READY 存在");
            } else {
                push_check(
                    &mut checks,
                    "ready",
                    "fail",
                    format!("缺少 {}", ready_path.display()),
                );
            }

            if ready_path.is_file() {
                match generation::load(&ctx.root, &environment, state.generation) {
                    Ok(generation_value) => {
                        push_check(
                            &mut checks,
                            "generation",
                            "ok",
                            format!(
                                "{} 个包，{} 个命令",
                                generation_value.packages.len(),
                                generation_value.tools.len()
                            ),
                        );

                        let report = verify(ctx)?;
                        if report.failures.is_empty() {
                            push_check(
                                &mut checks,
                                "cas",
                                "ok",
                                format!("{} 个对象全部有效", report.objects_checked),
                            );
                        } else {
                            push_check(
                                &mut checks,
                                "cas",
                                "fail",
                                format!("{} 个对象失败", report.failures.len()),
                            );
                            for failure in &report.failures {
                                push_check(&mut checks, "cas", "fail", failure.clone());
                            }
                        }

                        let shim_dir = shims::shim_dir(&ctx.root);
                        let missing = generation_value
                            .tools
                            .keys()
                            .filter(|name| !shim_dir.join(format!("{name}.exe")).is_file())
                            .cloned()
                            .collect::<Vec<_>>();
                        if missing.is_empty() {
                            push_check(
                                &mut checks,
                                "shims",
                                "ok",
                                format!("{} 个 shim 就绪", generation_value.tools.len()),
                            );
                        } else if fix {
                            match shims::install_shims(&ctx.root, &generation_value) {
                                Ok(_) => {
                                    fixed.push("重建 shim 目录".to_string());
                                    push_check(
                                        &mut checks,
                                        "shims",
                                        "ok",
                                        format!("已重建 {} 个缺失 shim", missing.len()),
                                    );
                                }
                                Err(error) => {
                                    push_check(&mut checks, "shims", "fail", error.message)
                                }
                            }
                        } else {
                            push_check(
                                &mut checks,
                                "shims",
                                "fail",
                                format!(
                                    "缺少 {} 个 shim（{}）；运行 `kita doctor --fix`",
                                    missing.len(),
                                    missing.join(", ")
                                ),
                            );
                        }
                    }
                    Err(error) => push_check(&mut checks, "generation", "fail", error.message),
                }
            }
        }
    }

    match lock::read_optional(&ctx.lock_path()) {
        Ok(Some(lock_file)) => match load_project(&ctx.project_path()) {
            Ok(project) => match canonical_digest(&project) {
                Ok(digest) => {
                    if lock_file.manifest == digest {
                        push_check(
                            &mut checks,
                            "lock",
                            "ok",
                            format!("{} 个包", lock_file.packages.len()),
                        );
                    } else {
                        push_check(
                            &mut checks,
                            "lock",
                            "warn",
                            "kita.toml 与 kita.lock 不一致；运行 `kita sync`",
                        );
                    }
                }
                Err(error) => push_check(&mut checks, "lock", "fail", error.message),
            },
            Err(error) => push_check(
                &mut checks,
                "lock",
                "warn",
                format!("无法读取 kita.toml：{}", error.message),
            ),
        },
        Ok(None) => push_check(&mut checks, "lock", "warn", "当前目录没有 kita.lock"),
        Err(error) => push_check(&mut checks, "lock", "fail", error.message),
    }

    Ok(DoctorReport { checks, fixed })
}

#[derive(Serialize)]
pub struct GcOutcome {
    pub dry_run: bool,
    pub environment: String,
    pub kept_generations: Vec<u64>,
    pub trees_total: usize,
    pub trees_removed: usize,
    pub objects_total: usize,
    pub objects_removed: usize,
    pub bytes_removed: u64,
}

fn read_hex_entries(directory: &Path) -> Result<Vec<(Sha256, u64, PathBuf)>> {
    let mut items = Vec::new();
    let read_dir = match fs::read_dir(directory) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(items),
        Err(error) => {
            return Err(KitaError::io(
                "KITA-2001",
                format!("无法读取 {}", directory.display()),
                error,
            ))
        }
    };
    for entry in read_dir {
        let entry = entry.map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法读取 {}", directory.display()),
                error,
            )
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        let Ok(digest) = Sha256::parse_hex(stem) else {
            continue;
        };
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        items.push((digest, size, path));
    }
    Ok(items)
}

fn generation_references(
    kita_root: &Path,
    environment: &str,
    numbers: &[u64],
    store: &Store,
) -> Result<(BTreeSet<Sha256>, BTreeSet<Sha256>)> {
    let mut trees = BTreeSet::new();
    let mut objects = BTreeSet::new();
    for number in numbers {
        let Ok(generation_value) = generation::load(kita_root, environment, *number) else {
            continue;
        };
        for package in &generation_value.packages {
            trees.insert(package.tree);
            if let Ok(tree) = store.read_tree(&package.tree) {
                for entry in tree.entries {
                    objects.insert(entry.sha256);
                }
            }
        }
    }
    Ok((trees, objects))
}

pub fn gc(ctx: &Context, dry_run: bool) -> Result<GcOutcome> {
    let active = ActiveState::load(&ctx.root)?.ok_or_else(|| {
        KitaError::new("KITA-2002", "当前没有活动环境").with_hint("先运行 `kita sync`")
    })?;
    let environment = environment_from_active(&active)?;
    let mut kept = vec![active.generation];
    if let Some(previous) = generation::previous(&ctx.root, &environment, active.generation)? {
        kept.push(previous);
    }
    for number in generation::load_pins(&ctx.root, &environment)?.generation {
        kept.push(number);
    }
    kept.sort_unstable();
    kept.dedup();

    let store = Store::new(&ctx.root);
    let (mut kept_trees, mut kept_objects) =
        generation_references(&ctx.root, &environment, &kept, &store)?;
    if let Some(lock_file) = lock::read_optional(&ctx.lock_path())? {
        for package in &lock_file.packages {
            kept_trees.insert(package.tree);
            if let Ok(tree) = store.read_tree(&package.tree) {
                for entry in tree.entries {
                    kept_objects.insert(entry.sha256);
                }
            }
        }
    }

    let trees = read_hex_entries(&store.root().join("trees"))?;
    let objects = read_hex_entries(&store.root().join("objects"))?;
    let mut trees_removed = 0usize;
    let mut objects_removed = 0usize;
    let mut bytes_removed = 0u64;

    for (digest, _size, path) in &trees {
        if kept_trees.contains(digest) {
            continue;
        }
        trees_removed += 1;
        if !dry_run {
            let _ = fs::remove_file(path);
        }
    }
    for (digest, size, path) in &objects {
        if kept_objects.contains(digest) {
            continue;
        }
        objects_removed += 1;
        bytes_removed += size;
        if !dry_run {
            let _ = fs::remove_file(path);
        }
    }

    Ok(GcOutcome {
        dry_run,
        environment,
        kept_generations: kept,
        trees_total: trees.len(),
        trees_removed,
        objects_total: objects.len(),
        objects_removed,
        bytes_removed,
    })
}

#[derive(Serialize)]
pub struct CacheReport {
    pub path: String,
    pub files: usize,
    pub bytes: u64,
}

pub fn cache_report(ctx: &Context) -> Result<CacheReport> {
    let directory = ctx.root.join("cache").join("downloads");
    let mut files = 0usize;
    let mut bytes = 0u64;
    if let Ok(read_dir) = fs::read_dir(&directory) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_file() {
                files += 1;
                bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            }
        }
    }
    Ok(CacheReport {
        path: directory.to_string_lossy().to_string(),
        files,
        bytes,
    })
}

#[derive(Serialize)]
pub struct CacheCleanOutcome {
    pub removed: usize,
    pub bytes: u64,
    pub kept: usize,
}

pub fn cache_clean(ctx: &Context, all: bool) -> Result<CacheCleanOutcome> {
    let directory = ctx.root.join("cache").join("downloads");
    let keep: BTreeSet<String> = if all {
        BTreeSet::new()
    } else {
        lock::read_optional(&ctx.lock_path())?
            .map(|lock_file| {
                lock_file
                    .packages
                    .iter()
                    .map(|package| package.artifact.sha256.to_hex())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default()
    };

    let mut removed = 0usize;
    let mut bytes = 0u64;
    let mut kept = 0usize;
    if let Ok(read_dir) = fs::read_dir(&directory) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if keep.contains(&name) {
                kept += 1;
                continue;
            }
            let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            if fs::remove_file(&path).is_ok() {
                removed += 1;
                bytes += size;
            }
        }
    }
    Ok(CacheCleanOutcome {
        removed,
        bytes,
        kept,
    })
}

#[derive(Serialize)]
pub struct PinOutcome {
    pub environment: String,
    pub generation: u64,
    pub pinned: bool,
    pub changed: bool,
}

pub fn pin_generation(ctx: &Context, number: u64, pinned: bool) -> Result<PinOutcome> {
    let active = ActiveState::load(&ctx.root)?;
    let environment = match &active {
        Some(state) => environment_from_active(state)?,
        None => "default".to_string(),
    };

    if pinned {
        generation::load(&ctx.root, &environment, number).map_err(|_| {
            KitaError::new(
                "KITA-2002",
                format!("generation {number} 不存在或未完成"),
            )
            .with_hint("运行 `kita generation list` 查看已完成的 generation")
        })?;
    }

    let mut pins = generation::load_pins(&ctx.root, &environment)?;
    let changed = if pinned {
        if pins.generation.contains(&number) {
            false
        } else {
            pins.generation.push(number);
            pins.generation.sort_unstable();
            pins.generation.dedup();
            generation::save_pins(&ctx.root, &environment, &pins)?;
            true
        }
    } else {
        let before = pins.generation.len();
        pins.generation.retain(|value| *value != number);
        if pins.generation.len() == before {
            false
        } else {
            generation::save_pins(&ctx.root, &environment, &pins)?;
            true
        }
    };

    Ok(PinOutcome {
        environment,
        generation: number,
        pinned,
        changed,
    })
}

#[derive(Serialize)]
pub struct GenerationGcOutcome {
    pub environment: String,
    pub kept: Vec<u64>,
    pub removed: Vec<u64>,
}

pub fn generation_gc(ctx: &Context) -> Result<GenerationGcOutcome> {
    let active = ActiveState::load(&ctx.root)?;
    let environment = match &active {
        Some(state) => environment_from_active(state)?,
        None => "default".to_string(),
    };

    let mut keep: BTreeSet<u64> = generation::load_pins(&ctx.root, &environment)?
        .generation
        .into_iter()
        .collect();
    if let Some(state) = &active {
        keep.insert(state.generation);
        if let Some(previous) =
            generation::previous(&ctx.root, &environment, state.generation)?
        {
            keep.insert(previous);
        }
    }
    if keep.is_empty() {
        let latest = generation::latest_number(&ctx.root, &environment)?;
        if latest > 0 {
            keep.insert(latest);
        }
    }

    let mut removed = Vec::new();
    for number in generation::list(&ctx.root, &environment)? {
        if keep.contains(&number) {
            continue;
        }
        let directory = generation::generation_dir(&ctx.root, &environment, number);
        fs::remove_dir_all(&directory).map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法删除 {}", directory.display()),
                error,
            )
        })?;
        removed.push(number);
    }

    Ok(GenerationGcOutcome {
        environment,
        kept: keep.into_iter().collect(),
        removed,
    })
}

