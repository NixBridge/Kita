use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::digest::Sha256;
use crate::generation::Generation;
use crate::id::PackageId;
use crate::resolver::Resolution;

#[derive(Clone, Debug, Serialize)]
pub struct PackageChange {
    pub id: String,
    /// `add`, `remove`, `upgrade`, `downgrade`, or `relink`.
    pub action: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Download {
    pub id: String,
    pub sha256: String,
    pub size: u64,
    pub cached: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub platform: String,
    pub generation: u64,
    pub changes: Vec<PackageChange>,
    pub downloads: Vec<Download>,
    pub manifest_changed: bool,
    pub requires_build: bool,
    pub warnings: Vec<String>,
}

/// Pure diff between the resolved graph and the current generation.
pub fn build_plan(
    platform: &str,
    planned_generation: u64,
    current: Option<&Generation>,
    resolution: &Resolution,
    trees: &BTreeMap<PackageId, Sha256>,
    downloads: Vec<Download>,
    manifest_digest: Sha256,
) -> Plan {
    let mut current_packages: BTreeMap<PackageId, (String, Sha256)> = BTreeMap::new();
    if let Some(current) = current {
        for package in &current.packages {
            current_packages.insert(
                package.id.clone(),
                (package.version.to_string(), package.tree),
            );
        }
    }

    let mut changes = Vec::new();
    for package in resolution.iter() {
        let id = package.id.canonical();
        match current_packages.remove(&package.id) {
            None => changes.push(PackageChange {
                id,
                action: "add".to_string(),
                from: None,
                to: Some(package.version.to_string()),
            }),
            Some((old_version, old_tree)) => {
                let new_version = package.version.to_string();
                let action = if old_version != new_version {
                    if package.version > crate::version::Version::parse(&old_version).unwrap_or_else(|_| package.version.clone()) {
                        "upgrade"
                    } else {
                        "downgrade"
                    }
                } else if trees
                    .get(&package.id)
                    .map(|tree| tree != &old_tree)
                    .unwrap_or(false)
                {
                    "relink"
                } else {
                    continue;
                };
                changes.push(PackageChange {
                    id,
                    action: action.to_string(),
                    from: Some(old_version),
                    to: Some(new_version),
                });
            }
        }
    }
    for (id, (version, _)) in current_packages {
        changes.push(PackageChange {
            id: id.canonical(),
            action: "remove".to_string(),
            from: Some(version),
            to: None,
        });
    }
    changes.sort_by(|left, right| left.id.cmp(&right.id));

    let manifest_changed = current
        .map(|generation| generation.manifest != manifest_digest)
        .unwrap_or(true);
    let missing_tree = resolution
        .iter()
        .any(|package| !trees.contains_key(&package.id));
    let requires_build =
        current.is_none() || manifest_changed || !changes.is_empty() || missing_tree;

    let mut warnings = Vec::new();
    if !requires_build {
        warnings.push("无需变更".to_string());
    }

    Plan {
        platform: platform.to_string(),
        generation: planned_generation,
        changes,
        downloads,
        manifest_changed,
        requires_build,
        warnings,
    }
}

impl Plan {
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.requires_build {
            let _ = writeln!(
                out,
                "计划：为 {} 构建 generation {}",
                self.platform, self.generation
            );
        } else {
            let _ = writeln!(out, "环境已是最新（generation {}）", self.generation);
        }
        if self.manifest_changed && self.changes.is_empty() {
            let _ = writeln!(out, "  ~ kita.toml 已变更");
        }
        for change in &self.changes {
            let symbol = match change.action.as_str() {
                "add" => "+",
                "remove" => "-",
                "upgrade" => "↑",
                "downgrade" => "↓",
                _ => "~",
            };
            match (&change.from, &change.to) {
                (_, None) => {
                    let _ = writeln!(
                        out,
                        "  {symbol} 移除 {} {}",
                        change.id,
                        change.from.as_deref().unwrap_or("?")
                    );
                }
                (None, Some(to)) => {
                    let _ = writeln!(out, "  {symbol} 添加 {} {to}", change.id);
                }
                (Some(from), Some(to)) if from == to => {
                    let _ = writeln!(out, "  {symbol} 重建 {} {to} 的文件树", change.id);
                }
                (Some(from), Some(to)) => {
                    let _ = writeln!(out, "  {symbol} {} {from} → {to}", change.id);
                }
            }
        }
        if !self.downloads.is_empty() {
            let _ = writeln!(out, "需要下载：");
            for download in &self.downloads {
                let state = if download.cached { "已缓存" } else { "缺失" };
                let _ = writeln!(
                    out,
                    "  {} {} ({}, {state})",
                    download.id,
                    &download.sha256[..12.min(download.sha256.len())],
                    human_bytes(download.size)
                );
            }
        }
        for warning in &self.warnings {
            let _ = writeln!(out, "提示：{warning}");
        }
        out
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
    }
}
