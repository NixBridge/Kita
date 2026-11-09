use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table};

use crate::digest::{sha256_bytes, Sha256};
use crate::error::{KitaError, Result};
use crate::id::PackageId;
use crate::path::RelPath;
use crate::requirement::Requirement;
use crate::util::{expand_template, is_valid_command_name};
use crate::version::Version;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    User,
    Machine,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::User => f.write_str("user"),
            Scope::Machine => f.write_str("machine"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BinKind {
    #[default]
    Console,
    Gui,
}

impl std::fmt::Display for BinKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinKind::Console => f.write_str("console"),
            BinKind::Gui => f.write_str("gui"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Elevation {
    #[default]
    Never,
    Optional,
    Required,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSpec {
    #[serde(default = "default_environment_name")]
    pub name: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

fn default_environment_name() -> String {
    "default".to_string()
}

impl Default for EnvironmentSpec {
    fn default() -> Self {
        Self {
            name: default_environment_name(),
            scope: Scope::User,
            platform: None,
            channel: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    pub schema: u32,
    #[serde(default)]
    pub environment: EnvironmentSpec,
    pub packages: BTreeMap<String, PackageRequest>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageRequest {
    Simple(String),
    Detailed {
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<Scope>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct NormalizedProject {
    pub schema: u32,
    pub environment: EnvironmentSpec,
    pub packages: BTreeMap<PackageId, ResolvedRequest>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedRequest {
    pub requirement: Requirement,
    pub scope: Scope,
    pub source: String,
}

impl ProjectManifest {
    pub fn normalize(self) -> Result<NormalizedProject> {
        if self.schema != 1 {
            return Err(KitaError::new(
                "KITA-1002",
                format!("不支持的 kita.toml schema：{}", self.schema),
            ));
        }
        if let Some(platform) = &self.environment.platform {
            if platform != "x86_64-pc-windows-msvc" && platform != "aarch64-pc-windows-msvc" {
                return Err(KitaError::new(
                    "KITA-9002",
                    format!("清单中的 platform `{platform}` 不是受支持的 Windows 目标"),
                ));
            }
        }

        let mut packages = BTreeMap::new();
        for (raw_id, request) in self.packages {
            let id = PackageId::parse(&raw_id)?;
            if packages.contains_key(&id) {
                return Err(KitaError::new(
                    "KITA-1002",
                    format!("包 ID `{id}` 在大小写折叠后重复出现"),
                ));
            }

            let (raw_requirement, scope_override, source) = match request {
                PackageRequest::Simple(requirement) => (requirement, None, None),
                PackageRequest::Detailed {
                    version,
                    scope,
                    source,
                } => (version, scope, source),
            };

            let requirement = Requirement::parse(&raw_requirement).map_err(|error| {
                error.with_hint(format!("检查 kita.toml 中 `{id}` 的版本需求"))
            })?;
            let scope = scope_override.unwrap_or(self.environment.scope);
            let source = source.unwrap_or_else(|| "kita".to_string());
            packages.insert(
                id,
                ResolvedRequest {
                    requirement,
                    scope,
                    source,
                },
            );
        }

        validate_env_map(&self.env, false)?;

        Ok(NormalizedProject {
            schema: self.schema,
            environment: self.environment,
            packages,
            env: self.env,
        })
    }
}

fn validate_env_map(env: &BTreeMap<String, String>, allow_install_dir: bool) -> Result<()> {
    let variables: &[(&str, &str)] = if allow_install_dir {
        &[
            ("KITA_ROOT", "\\"),
            ("KITA_DATA", "\\"),
            ("KITA_PACKAGE", "pkg"),
            ("KITA_GENERATION", "1"),
            ("INSTALL_DIR", "\\"),
        ]
    } else {
        &[
            ("KITA_ROOT", "\\"),
            ("KITA_DATA", "\\"),
            ("KITA_PACKAGE", "pkg"),
            ("KITA_GENERATION", "1"),
        ]
    };

    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(KitaError::new(
                "KITA-1003",
                format!("非法环境变量名 `{key}`"),
            ));
        }
        if key.starts_with("KITA_") {
            return Err(KitaError::new(
                "KITA-1003",
                format!("环境变量 `{key}` 使用了 Kita 保留前缀"),
            ));
        }
        if value.contains('\0') {
            return Err(KitaError::new(
                "KITA-1003",
                format!("环境变量 `{key}` 的值包含 NUL"),
            ));
        }
        expand_template(value, variables).map_err(|error| {
            error.with_hint(format!("检查环境变量 `{key}` 的模板"))
        })?;
    }
    Ok(())
}

pub fn load_project(path: &Path) -> Result<NormalizedProject> {
    let bytes = fs::read(path).map_err(|error| {
        KitaError::io(
            "KITA-1001",
            format!("无法读取 {}", path.display()),
            error,
        )
        .with_hint("先运行 `kita init` 创建 kita.toml")
    })?;
    parse_project(&bytes).map_err(|error| {
        if error.hints.is_empty() {
            error.with_hint(format!("检查 {}", path.display()))
        } else {
            error
        }
    })
}

pub fn parse_project(bytes: &[u8]) -> Result<NormalizedProject> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        KitaError::new("KITA-1002", format!("kita.toml 不是 UTF-8：{error}"))
    })?;
    let manifest: ProjectManifest = toml::from_str(text).map_err(|error| {
        KitaError::new("KITA-1002", format!("kita.toml 解析失败：{error}"))
    })?;
    manifest.normalize()
}

#[derive(Serialize)]
struct CanonicalProject<'a> {
    schema: u32,
    environment: &'a EnvironmentSpec,
    packages: BTreeMap<String, CanonicalRequest>,
    env: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct CanonicalRequest {
    version: String,
    scope: Scope,
    source: String,
}

pub fn canonical_digest(project: &NormalizedProject) -> Result<Sha256> {
    let packages = project
        .packages
        .iter()
        .map(|(id, request)| {
            (
                id.canonical(),
                CanonicalRequest {
                    version: request.requirement.to_string(),
                    scope: request.scope,
                    source: request.source.clone(),
                },
            )
        })
        .collect();

    let canonical = CanonicalProject {
        schema: project.schema,
        environment: &project.environment,
        packages,
        env: &project.env,
    };
    let text = toml::to_string(&canonical)
        .map_err(|error| KitaError::new("KITA-1002", format!("规范化清单失败：{error}")))?;
    Ok(sha256_bytes(text.as_bytes()))
}

/// Parse `<id>[@<requirement>]`. Missing requirements become `*`.
pub fn parse_add_spec(input: &str) -> Result<(PackageId, String)> {
    let input = input.trim();
    if input.is_empty() {
        return Err(KitaError::new("KITA-1001", "包规格不能为空"));
    }
    let (id_part, requirement) = match input.split_once('@') {
        Some((id, requirement)) => (id.trim(), requirement.trim()),
        None => (input, "*"),
    };
    let id = PackageId::parse(id_part)?;
    if requirement.is_empty() {
        return Err(KitaError::new("KITA-3002", "`@` 后缺少版本需求"));
    }
    Requirement::parse(requirement)?;
    Ok((id, requirement.to_string()))
}

pub fn add_packages(path: &Path, specs: &[(PackageId, String)]) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|error| {
        KitaError::io(
            "KITA-1001",
            format!("无法读取 {}", path.display()),
            error,
        )
    })?;
    let mut document: DocumentMut = text.parse().map_err(|error| {
        KitaError::new("KITA-1002", format!("kita.toml 解析失败：{error}"))
    })?;

    if !document.contains_key("packages") {
        let mut table = Table::new();
        table.set_implicit(false);
        document.insert("packages", Item::Table(table));
    }
    let table = document
        .get_mut("packages")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| KitaError::new("KITA-1002", "`packages` 必须是表"))?;

    for (id, requirement) in specs {
        table.insert(&id.canonical(), toml_edit::value(requirement.as_str()));
    }

    let updated = document.to_string();
    parse_project(updated.as_bytes()).map_err(|error| {
        error.with_hint("拒绝写入会让清单非法的修改")
    })?;

    crate::util::atomic_write(path, updated.as_bytes())
}

pub fn remove_packages(path: &Path, ids: &[PackageId]) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|error| {
        KitaError::io(
            "KITA-1001",
            format!("无法读取 {}", path.display()),
            error,
        )
    })?;
    let mut document: DocumentMut = text.parse().map_err(|error| {
        KitaError::new("KITA-1002", format!("kita.toml 解析失败：{error}"))
    })?;

    let table = document
        .get_mut("packages")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| KitaError::new("KITA-1002", "`packages` 必须是表"))?;

    let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    let mut removed = 0usize;
    for key in keys {
        let Ok(id) = PackageId::parse(&key) else {
            continue;
        };
        if ids.contains(&id) {
            table.remove(&key);
            removed += 1;
        }
    }

    if removed == 0 {
        return Err(KitaError::new(
            "KITA-1004",
            "要移除的包不在 kita.toml 中",
        ));
    }

    let updated = document.to_string();
    parse_project(updated.as_bytes()).map_err(|error| {
        error.with_hint("拒绝写入会让清单非法的修改")
    })?;

    crate::util::atomic_write(path, updated.as_bytes())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema: u32,
    pub id: PackageId,
    pub version: Version,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    pub license: String,
    pub homepage: String,
    #[serde(default)]
    pub upstream: Option<String>,
    pub platform: PlatformSpec,
    pub install: InstallSpec,
    pub capabilities: Capabilities,
    #[serde(rename = "artifact")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, rename = "bin")]
    pub bins: Vec<BinSpec>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum InstallSpec {
    Portable {
        #[serde(default)]
        strip_components: u32,
        #[serde(default)]
        mutable: bool,
    },
    Native {
        kind: String,
        #[serde(default)]
        scope: Option<Scope>,
        #[serde(default)]
        upgrade_code: Option<String>,
        #[serde(default)]
        product_code: Option<String>,
        #[serde(default)]
        properties: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub elevation: Elevation,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub registry: bool,
    #[serde(default)]
    pub services: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            elevation: Elevation::Never,
            network: false,
            registry: false,
            services: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: String,
    pub url: String,
    pub sha256: Sha256,
    pub size: u64,
    pub format: String,
    #[serde(default)]
    pub strip_components: Option<u32>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub require_signature: Option<bool>,
}

impl Artifact {
    pub fn strip_components(&self, install_default: u32) -> u32 {
        self.strip_components.unwrap_or(install_default)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinSpec {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub kind: BinKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl BinSpec {
    pub fn commands(&self) -> Vec<String> {
        let mut commands = Vec::with_capacity(1 + self.aliases.len());
        commands.push(self.name.clone());
        commands.extend(self.aliases.iter().cloned());
        commands
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Simple(String),
    Detailed {
        version: String,
        #[serde(default)]
        bins: Option<bool>,
    },
}

impl DependencySpec {
    pub fn requirement(&self) -> &str {
        match self {
            DependencySpec::Simple(requirement) => requirement,
            DependencySpec::Detailed { version, .. } => version,
        }
    }

    pub fn bins(&self) -> bool {
        match self {
            DependencySpec::Simple(_) => true,
            DependencySpec::Detailed { bins, .. } => bins.unwrap_or(true),
        }
    }
}

pub fn load_package(bytes: &[u8]) -> Result<PackageManifest> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        KitaError::new("KITA-1002", format!("包清单不是 UTF-8：{error}"))
    })?;
    let manifest: PackageManifest = toml::from_str(text).map_err(|error| {
        KitaError::new("KITA-1002", format!("包清单解析失败：{error}"))
    })?;
    manifest.validate()?;
    Ok(manifest)
}

impl PackageManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(KitaError::new(
                "KITA-1002",
                format!("不支持的包清单 schema：{}", self.schema),
            ));
        }
        if self.summary.trim().is_empty() || self.summary.chars().count() > 120 {
            return Err(KitaError::new(
                "KITA-1003",
                "summary 必须为 1..=120 个字符",
            ));
        }
        if self.license.trim().is_empty() {
            return Err(KitaError::new("KITA-1003", "license 不能为空"));
        }
        if !self.homepage.starts_with("https://") {
            return Err(KitaError::new(
                "KITA-1003",
                "homepage 必须是 HTTPS URL",
            ));
        }
        if self.platform.arch.is_empty() {
            return Err(KitaError::new("KITA-1003", "platform.arch 不能为空"));
        }
        for arch in &self.platform.arch {
            if arch != "x86_64" && arch != "aarch64" {
                return Err(KitaError::new(
                    "KITA-9002",
                    format!("不支持的架构 `{arch}`"),
                ));
            }
        }

        if self.artifacts.is_empty() {
            return Err(KitaError::new(
                "KITA-1003",
                "至少需要一个 [[artifact]]",
            ));
        }
        let mut targets = BTreeSet::new();
        for artifact in &self.artifacts {
            if !targets.insert(artifact.target.clone()) {
                return Err(KitaError::new(
                    "KITA-1003",
                    format!("artifact target `{}` 重复", artifact.target),
                ));
            }
            let arch = artifact
                .target
                .split('-')
                .next()
                .unwrap_or_default();
            if !self.platform.arch.iter().any(|item| item == arch) {
                return Err(KitaError::new(
                    "KITA-9002",
                    format!(
                        "artifact target `{}` 不在 platform.arch 中",
                        artifact.target
                    ),
                ));
            }
            if artifact.size == 0 {
                return Err(KitaError::new(
                    "KITA-1003",
                    format!("artifact `{}` 的 size 必须大于 0", artifact.target),
                ));
            }
            if artifact.url.trim().is_empty() {
                return Err(KitaError::new(
                    "KITA-1003",
                    format!("artifact `{}` 缺少 url", artifact.target),
                ));
            }
            match &self.install {
                InstallSpec::Portable { .. } => {
                    if !matches!(artifact.format.as_str(), "zip" | "kta") {
                        return Err(KitaError::new(
                            "KITA-1003",
                            format!(
                                "当前构建只支持 portable zip/kta，artifact 使用了 `{}`",
                                artifact.format
                            ),
                        ));
                    }
                }
                InstallSpec::Native { kind, .. } => {
                    if kind != "msi" || artifact.format != "msi" {
                        return Err(KitaError::new(
                            "KITA-8001",
                            "native 安装只支持 MSI",
                        ));
                    }
                }
            }
        }

        match &self.install {
            InstallSpec::Portable { .. } => {
                if self.capabilities.elevation != Elevation::Never {
                    return Err(KitaError::new(
                        "KITA-1003",
                        "portable 包的 elevation 必须是 never",
                    ));
                }
                if self.capabilities.registry || self.capabilities.services {
                    return Err(KitaError::new(
                        "KITA-1003",
                        "portable 包不能声明 registry 或 services",
                    ));
                }
            }
            InstallSpec::Native { .. } => {}
        }

        let mut names = BTreeSet::new();
        for bin in &self.bins {
            for command in bin.commands() {
                if !is_valid_command_name(&command) {
                    return Err(KitaError::new(
                        "KITA-1003",
                        format!("命令名 `{command}` 不合法"),
                    ));
                }
                if !names.insert(command.to_ascii_lowercase()) {
                    return Err(KitaError::new(
                        "KITA-1003",
                        format!("命令名 `{command}` 在同一包内重复"),
                    ));
                }
            }
            let path = RelPath::parse(&bin.path).map_err(|error| {
                error.with_hint(format!("检查命令 `{}` 的 path", bin.name))
            })?;
            if matches!(&self.install, InstallSpec::Portable { .. })
                && path.as_str().contains("${")
            {
                return Err(KitaError::new(
                    "KITA-1003",
                    "portable 包的 bin.path 不能使用模板变量",
                ));
            }
            for (key, value) in &bin.env {
                validate_env_map(
                    &BTreeMap::from([(key.clone(), value.clone())]),
                    matches!(&self.install, InstallSpec::Native { .. }),
                )?;
            }
        }

        validate_env_map(
            &self.env,
            matches!(&self.install, InstallSpec::Native { .. }),
        )?;

        for (raw_id, dependency) in &self.dependencies {
            let id = PackageId::parse(raw_id)?;
            if id == self.id {
                return Err(KitaError::new(
                    "KITA-1003",
                    "包不能依赖自身",
                ));
            }
            Requirement::parse(dependency.requirement()).map_err(|error| {
                error.with_hint(format!("检查依赖 `{id}` 的版本需求"))
            })?;
        }

        Ok(())
    }

    pub fn artifact_for(&self, target: &str) -> Result<&Artifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.target == target)
            .ok_or_else(|| {
                let available = self
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.target.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                KitaError::new(
                    "KITA-9002",
                    format!(
                        "包 {} {} 没有适用于 {target} 的构建（可用：{available}）",
                        self.id, self.version
                    ),
                )
            })
    }

    pub fn install_strip_components(&self) -> u32 {
        match &self.install {
            InstallSpec::Portable {
                strip_components, ..
            } => *strip_components,
            InstallSpec::Native { .. } => 0,
        }
    }

    pub fn dependency_requirements(&self) -> Result<Vec<(PackageId, Requirement, bool)>> {
        let mut dependencies = Vec::new();
        for (raw_id, spec) in &self.dependencies {
            let id = PackageId::parse(raw_id)?;
            let requirement = Requirement::parse(spec.requirement())?;
            dependencies.push((id, requirement, spec.bins()));
        }
        Ok(dependencies)
    }

    pub fn exposed_bins(&self) -> Vec<String> {
        let mut bins = self
            .bins
            .iter()
            .flat_map(BinSpec::commands)
            .collect::<Vec<_>>();
        bins.sort();
        bins.dedup();
        bins
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical_digest, parse_add_spec, parse_project};

    const PROJECT: &str = r#"
schema = 1

[environment]
name = "dev"
scope = "user"

[packages]
ripgrep = "14.1"
"kita/cmake" = { version = ">=3.28, <4" }

[env]
EDITOR = "hx"
"#;

    #[test]
    fn normalizes_project() {
        let project = parse_project(PROJECT.as_bytes()).unwrap();
        assert_eq!(project.packages.len(), 2);
        assert!(project.packages.contains_key(&crate::id::PackageId::parse("ripgrep").unwrap()));
    }

    #[test]
    fn canonical_digest_ignores_comments_and_order() {
        let first = parse_project(PROJECT.as_bytes()).unwrap();
        let second = parse_project(
            r#"
# a different comment
schema = 1
[env]
EDITOR = "hx"
[packages]
"kita/cmake" = { version = ">=3.28, <4" }
ripgrep = "14.1"
[environment]
scope = "user"
name = "dev"
"#
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            canonical_digest(&first).unwrap(),
            canonical_digest(&second).unwrap()
        );
    }

    #[test]
    fn parses_add_specs() {
        let (id, requirement) = parse_add_spec("ripgrep@14.1").unwrap();
        assert_eq!(id.canonical(), "kita/ripgrep");
        assert_eq!(requirement, "14.1");
        let (_, requirement) = parse_add_spec("ripgrep").unwrap();
        assert_eq!(requirement, "*");
    }
}
