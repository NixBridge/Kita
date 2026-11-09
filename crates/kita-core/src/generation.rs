use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::active::{ActiveState, ToolEntry};
use crate::digest::Sha256;
use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::manifest::{BinKind, Scope};
use crate::resolver::ResolvedPackage;
use crate::store::Store;
use crate::util::{
    atomic_write, ensure_dir, expand_template, now_rfc3339, read_json_opt, slug, unique_suffix,
    write_json_atomic,
};
use crate::version::Version;

pub const GENERATION_SCHEMA: u32 = 1;
pub const READY_FILE: &str = "READY";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub schema: u32,
    pub generation: u64,
    pub created_at: String,
    pub platform: String,
    pub plan_digest: Sha256,
    pub manifest: Sha256,
    pub packages: Vec<GenerationPackage>,
    pub tools: BTreeMap<String, ToolEntry>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationPackage {
    pub id: PackageId,
    pub version: Version,
    pub instance: String,
    pub tree: Sha256,
    pub scope: Scope,
    pub activation: String,
}

/// One package plus the tree digest that was installed into the CAS.
pub struct BuildPackage<'a> {
    pub package: &'a ResolvedPackage,
    pub tree: Sha256,
    pub activation: &'a str,
}

pub fn env_root(kita_root: &Path, environment: &str) -> PathBuf {
    kita_root.join("envs").join(slug(environment))
}

pub fn generations_dir(kita_root: &Path, environment: &str) -> PathBuf {
    env_root(kita_root, environment).join("generations")
}

pub fn generation_dir(kita_root: &Path, environment: &str, generation: u64) -> PathBuf {
    generations_dir(kita_root, environment).join(generation.to_string())
}

pub fn generation_path(environment: &str, generation: u64) -> String {
    format!(
        "envs/{}/generations/{generation}",
        slug(environment)
    )
}

pub fn pins_path(kita_root: &Path, environment: &str) -> PathBuf {
    env_root(kita_root, environment).join("pins.json")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    #[serde(default)]
    pub generation: Vec<u64>,
}

pub fn load_pins(kita_root: &Path, environment: &str) -> Result<Pins> {
    Ok(read_json_opt(&pins_path(kita_root, environment))?.unwrap_or_default())
}

pub fn save_pins(kita_root: &Path, environment: &str, pins: &Pins) -> Result<()> {
    write_json_atomic(&pins_path(kita_root, environment), pins)
}

pub fn instance_name(package: &ResolvedPackage, tree: &Sha256) -> String {
    let hex = tree.to_hex();
    slug(&format!(
        "{} {} {}",
        package.id.canonical(),
        package.version,
        &hex[..8]
    ))
}

/// Highest existing generation number, or 0 when the environment is empty.
pub fn latest_number(kita_root: &Path, environment: &str) -> Result<u64> {
    let numbers = list(kita_root, environment)?;
    Ok(numbers.last().copied().unwrap_or(0))
}

pub fn list(kita_root: &Path, environment: &str) -> Result<Vec<u64>> {
    let directory = generations_dir(kita_root, environment);
    let read_dir = match fs::read_dir(&directory) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(KitaError::io(
                "KITA-2001",
                format!("无法读取 {}", directory.display()),
                error,
            ))
        }
    };
    let mut numbers = Vec::new();
    for item in read_dir {
        let item = item.map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法读取 {}", directory.display()),
                error,
            )
        })?;
        if !item.path().is_dir() {
            continue;
        }
        let name = item.file_name();
        if let Some(number) = name.to_str().and_then(|name| name.parse::<u64>().ok()) {
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    Ok(numbers)
}

pub fn load(kita_root: &Path, environment: &str, generation: u64) -> Result<Generation> {
    let directory = generation_dir(kita_root, environment, generation);
    if !directory.join(READY_FILE).is_file() {
        return Err(KitaError::new(
            "KITA-2002",
            format!(
                "generation {generation} 未完成（缺少 READY）：{}",
                directory.display()
            ),
        ));
    }
    let path = directory.join("generation.json");
    let generation: Generation = read_json_opt(&path)?.ok_or_else(|| {
        KitaError::new(
            "KITA-2002",
            format!("缺少 generation.json：{}", path.display()),
        )
    })?;
    if generation.schema != GENERATION_SCHEMA {
        return Err(KitaError::new(
            "KITA-2002",
            format!("不支持的 generation schema：{}", generation.schema),
        ));
    }
    Ok(generation)
}

/// Highest complete generation strictly below `current`.
pub fn previous(kita_root: &Path, environment: &str, current: u64) -> Result<Option<u64>> {
    let numbers = list(kita_root, environment)?;
    for number in numbers.into_iter().rev() {
        if number >= current {
            continue;
        }
        if load(kita_root, environment, number).is_ok() {
            return Ok(Some(number));
        }
    }
    Ok(None)
}

/// Build a generation in a staging directory, then rename it into place.
/// The READY marker is written before the rename so a crash never exposes a
/// partially built generation.
pub fn build(
    kita_root: &Path,
    environment: &str,
    platform: &str,
    manifest_digest: Sha256,
    plan_digest: Sha256,
    packages: &[BuildPackage<'_>],
    project_env: &BTreeMap<String, String>,
) -> Result<Generation> {
    let number = latest_number(kita_root, environment)? + 1;
    let store = Store::new(kita_root);
    let staging = env_root(kita_root, environment)
        .join("tmp")
        .join(format!("gen-{}", unique_suffix()));
    ensure_dir(&staging)?;

    let result = build_into(
        kita_root,
        environment,
        number,
        platform,
        manifest_digest,
        plan_digest,
        packages,
        project_env,
        &store,
        &staging,
    );

    let generation = match result {
        Ok(generation) => generation,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    let target = generation_dir(kita_root, environment, number);
    ensure_dir(&generations_dir(kita_root, environment))?;
    if target.exists() {
        let _ = fs::remove_dir_all(&staging);
        return Err(KitaError::new(
            "KITA-2001",
            format!("generation 目录已存在：{}", target.display()),
        ));
    }
    fs::rename(&staging, &target).map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        KitaError::io(
            "KITA-2001",
            format!("无法提交 generation {}", target.display()),
            error,
        )
    })?;

    Ok(generation)
}

#[allow(clippy::too_many_arguments)]
fn build_into(
    kita_root: &Path,
    environment: &str,
    number: u64,
    platform: &str,
    manifest_digest: Sha256,
    plan_digest: Sha256,
    packages: &[BuildPackage<'_>],
    project_env: &BTreeMap<String, String>,
    store: &Store,
    staging: &Path,
) -> Result<Generation> {
    let mut tools: BTreeMap<String, ToolEntry> = BTreeMap::new();
    let mut records = Vec::with_capacity(packages.len());
    let mut instances = BTreeSet::new();

    for layer in packages {
        let package = layer.package;
        let instance = instance_name(package, &layer.tree);
        if !instances.insert(instance.clone()) {
            return Err(KitaError::new(
                "KITA-2001",
                format!("instance 名称冲突：{instance}"),
            ));
        }
        let tree_manifest = store.read_tree(&layer.tree)?;
        let tree_root = staging.join("tree").join(&instance);
        store.materialize(&tree_manifest.entries, &tree_root, false)?;

        let generation_root = generation_dir(kita_root, environment, number);
        let instance_dir = generation_root.join("tree").join(&instance);
        let data_dir = kita_root
            .join("data")
            .join(package.id.namespace())
            .join(package.id.name());

        let exposed: BTreeSet<&str> = package.bins.iter().map(String::as_str).collect();
        for bin in &package.manifest.bins {
            let commands = bin.commands();
            let wanted = commands
                .iter()
                .any(|command| exposed.contains(command.as_str()));
            if !wanted {
                continue;
            }

            let mut env = package.manifest.env.clone();
            env.extend(bin.env.clone());
            let env = expand_env(
                &env,
                kita_root,
                &data_dir,
                &instance_dir,
                number,
            )?;

            let tool = ToolEntry {
                package: package.id.clone(),
                version: package.version.clone(),
                origin: "managed".to_string(),
                root: format!("tree/{instance}"),
                path: crate::path::RelPath::parse(&bin.path)?.as_str().to_string(),
                kind: match bin.kind {
                    BinKind::Console => BinKind::Console,
                    BinKind::Gui => BinKind::Gui,
                },
                env,
            };
            for command in commands {
                if !exposed.contains(command.as_str()) {
                    continue;
                }
                if let Some(existing) = tools.insert(command.clone(), tool.clone()) {
                    return Err(KitaError::new(
                        "KITA-9001",
                        format!(
                            "命令 `{command}` 同时由 {} 和 {} 提供",
                            existing.package, package.id
                        ),
                    ));
                }
            }
        }

        records.push(GenerationPackage {
            id: package.id.clone(),
            version: package.version.clone(),
            instance,
            tree: layer.tree,
            scope: package.scope,
            activation: layer.activation.to_string(),
        });
    }

    let generation_root = generation_dir(kita_root, environment, number);
    let data_dir = kita_root.join("data");
    let mut env = expand_env(
        project_env,
        kita_root,
        &data_dir,
        &generation_root,
        number,
    )?;
    env.insert("KITA_ACTIVE".to_string(), number.to_string());

    let generation = Generation {
        schema: GENERATION_SCHEMA,
        generation: number,
        created_at: now_rfc3339(),
        platform: platform.to_string(),
        plan_digest,
        manifest: manifest_digest,
        packages: records,
        tools,
        env,
    };

    write_json_atomic(&staging.join("generation.json"), &generation)?;
    atomic_write(&staging.join(READY_FILE), b"")?;
    Ok(generation)
}

fn expand_env(
    values: &BTreeMap<String, String>,
    kita_root: &Path,
    data_dir: &Path,
    package_dir: &Path,
    generation: u64,
) -> Result<BTreeMap<String, String>> {
    let root = kita_root.to_string_lossy().to_string();
    let data = data_dir.to_string_lossy().to_string();
    let package_path = package_dir.to_string_lossy().to_string();
    let generation_text = generation.to_string();
    let variables: &[(&str, &str)] = &[
        ("KITA_ROOT", &root),
        ("KITA_DATA", &data),
        ("KITA_PACKAGE", &package_path),
        ("KITA_GENERATION", &generation_text),
    ];
    let mut expanded = BTreeMap::new();
    for (key, value) in values {
        expanded.insert(key.clone(), expand_template(value, variables)?);
    }
    Ok(expanded)
}

pub fn active_state(
    kita_root: &Path,
    environment: &str,
    scope: Scope,
    platform: &str,
    generation: &Generation,
) -> ActiveState {
    let _ = kita_root;
    ActiveState {
        schema: 1,
        scope,
        generation: generation.generation,
        generation_path: generation_path(environment, generation.generation),
        platform: platform.to_string(),
        activated_at: now_rfc3339(),
        tools: generation.tools.clone(),
        env: generation.env.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::generation_path;

    #[test]
    fn generation_paths_are_relative() {
        assert_eq!(
            generation_path("default", 42),
            "envs/default/generations/42"
        );
    }
}
