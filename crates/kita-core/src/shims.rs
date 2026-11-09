use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{KitaError, Result};
use crate::generation::Generation;
use crate::manifest::BinKind;
use crate::util::{ensure_dir, is_valid_command_name, write_json_atomic};

pub const SHIM_CONSOLE: &str = "kita-shim.exe";
pub const SHIM_GUI: &str = "kita-shimw.exe";
pub const TOOLS_FILE: &str = "tools.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShimTool {
    pub package: String,
    pub version: String,
    pub kind: BinKind,
}

pub fn shim_dir(kita_root: &Path) -> PathBuf {
    kita_root.join("shims")
}

pub fn tools_path(kita_root: &Path) -> PathBuf {
    shim_dir(kita_root).join(TOOLS_FILE)
}

/// Locate the shim binaries that ship next to the running `kita` executable.
pub fn source_binaries() -> Result<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe().map_err(|error| {
        KitaError::io("KITA-9003", "无法定位当前可执行文件", error)
    })?;
    let directory = exe.parent().ok_or_else(|| {
        KitaError::new("KITA-9003", "当前可执行文件没有父目录")
    })?;
    let console = directory.join(SHIM_CONSOLE);
    let gui = directory.join(SHIM_GUI);
    for path in [&console, &gui] {
        if !path.is_file() {
            return Err(KitaError::new(
                "KITA-9003",
                format!("缺少 shim 二进制：{}", path.display()),
            )
            .with_hint("请与 kita.exe 一起构建 `apps/kita-shim`，或运行 `cargo build --release`"));
        }
    }
    Ok((console, gui))
}

fn copy_binary(source: &Path, target: &Path) -> Result<()> {
    if let (Ok(left), Ok(right)) = (fs::metadata(source), fs::metadata(target)) {
        if left.len() == right.len() {
            return Ok(());
        }
    }
    if let Some(parent) = target.parent() {
        ensure_dir(parent)?;
    }
    fs::copy(source, target).map_err(|error| {
        KitaError::io(
            "KITA-2001",
            format!(
                "无法复制 shim {} 到 {}",
                source.display(),
                target.display()
            ),
            error,
        )
    })?;
    Ok(())
}

/// Rebuild the shim directory for a generation: refresh shared binaries, link
/// one `<command>.exe` per tool, and remove stale managed shims.
pub fn install_shims(kita_root: &Path, generation: &Generation) -> Result<Vec<String>> {
    let (console, gui) = source_binaries()?;
    let directory = shim_dir(kita_root);
    ensure_dir(&directory)?;

    let console_target = directory.join(SHIM_CONSOLE);
    let gui_target = directory.join(SHIM_GUI);
    copy_binary(&console, &console_target)?;
    copy_binary(&gui, &gui_target)?;

    let mut warnings = Vec::new();
    for (name, tool) in &generation.tools {
        let target = directory.join(format!("{name}.exe"));
        let source = match tool.kind {
            BinKind::Console => &console_target,
            BinKind::Gui => &gui_target,
        };
        if target.exists() {
            let _ = fs::remove_file(&target);
        }
        if fs::hard_link(source, &target).is_err() {
            if let Err(error) = fs::copy(source, &target) {
                return Err(KitaError::io(
                    "KITA-2001",
                    format!("无法创建 shim {}", target.display()),
                    error,
                ));
            }
        }
    }

    // Remove shims for commands that no longer exist, but only files that look
    // like managed shims rather than arbitrary user files.
    let keep: BTreeSet<String> = generation.tools.keys().cloned().collect();
    if let Ok(read_dir) = fs::read_dir(&directory) {
        for item in read_dir.flatten() {
            let path = item.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name == SHIM_CONSOLE || file_name == SHIM_GUI || file_name == TOOLS_FILE {
                continue;
            }
            let Some(stem) = file_name.strip_suffix(".exe") else {
                continue;
            };
            if !is_valid_command_name(stem) || keep.contains(stem) {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                warnings.push(format!("已移除过期 shim {stem}"));
            }
        }
    }

    let tools = generation
        .tools
        .iter()
        .map(|(name, tool)| {
            (
                name.clone(),
                ShimTool {
                    package: tool.package.canonical(),
                    version: tool.version.to_string(),
                    kind: tool.kind,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    write_json_atomic(&tools_path(kita_root), &tools)?;

    Ok(warnings)
}

/// Remove `active.json`-unreferenced shims when no generation is active.
pub fn clear_shims(kita_root: &Path) -> Result<Vec<String>> {
    let directory = shim_dir(kita_root);
    let mut warnings = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&directory) {
        for item in read_dir.flatten() {
            let path = item.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if file_name == SHIM_CONSOLE || file_name == SHIM_GUI || file_name == TOOLS_FILE {
                continue;
            }
            let Some(stem) = file_name.strip_suffix(".exe") else {
                continue;
            };
            if is_valid_command_name(stem) && fs::remove_file(&path).is_ok() {
                warnings.push(format!("已移除 shim {stem}"));
            }
        }
    }
    Ok(warnings)
}
