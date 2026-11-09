use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct ActiveState {
    generation_path: String,
    #[serde(default)]
    tools: BTreeMap<String, ToolEntry>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ToolEntry {
    origin: String,
    root: String,
    path: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub fn run() -> i32 {
    match execute() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("kita shim: {message}");
            eprintln!("help: 运行 `kita sync` 或 `kita doctor` 修复本地状态");
            1
        }
    }
}

fn execute() -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|error| format!("无法定位自身路径：{error}"))?;
    let shim_dir = exe
        .parent()
        .ok_or_else(|| "shim 没有父目录".to_string())?;
    let kita_root = shim_dir
        .parent()
        .ok_or_else(|| "无法推导 Kita 根目录".to_string())?;

    let command_name = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "shim 文件名不是有效 UTF-8".to_string())?
        .to_ascii_lowercase();

    let active_path = kita_root.join("active.json");
    let bytes = fs::read(&active_path)
        .map_err(|error| format!("无法读取 {}：{error}", active_path.display()))?;
    let active: ActiveState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("active.json 解析失败：{error}（拒绝猜测路径）"))?;

    let tool = active
        .tools
        .get(&command_name)
        .ok_or_else(|| format!("未知命令 `{command_name}`；它不在当前 generation 中"))?;

    let target = resolve_target(kita_root, &active.generation_path, tool)?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let mut command = if is_script(&target) {
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

    let status = command
        .status()
        .map_err(|error| format!("无法启动 {}：{error}", target.display()))?;
    Ok(status.code().unwrap_or(1))
}

fn resolve_target(
    kita_root: &Path,
    generation_path: &str,
    tool: &ToolEntry,
) -> Result<PathBuf, String> {
    if tool.origin != "managed" {
        return Err(format!(
            "不支持的 origin `{}`；当前构建只支持 managed 工具",
            tool.origin
        ));
    }
    let relative_root = safe_relative(&tool.root)?;
    let relative_path = safe_relative(&tool.path)?;

    let generation_relative = generation_relative_path(generation_path)?;
    let generation_dir = kita_root.join(generation_relative);
    let canonical_generation = fs::canonicalize(&generation_dir)
        .map_err(|error| format!("无法解析 generation 目录：{error}"))?;

    let target = generation_dir.join(relative_root).join(relative_path);
    let canonical = fs::canonicalize(&target)
        .map_err(|error| format!("工具目标不存在或不可访问 {}：{error}", target.display()))?;
    if !canonical.starts_with(&canonical_generation) {
        return Err(format!("工具路径逃逸 generation：{}", canonical.display()));
    }
    Ok(canonical)
}

fn generation_relative_path(value: &str) -> Result<PathBuf, String> {
    let relative = safe_relative(value)?;
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.len() != 4 || components[0] != "envs" || components[2] != "generations" {
        return Err(format!("generation_path 非法：{value}"));
    }
    Ok(relative)
}

fn safe_relative(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains('\0') {
        return Err("路径为空或包含 NUL".to_string());
    }
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(format!("拒绝绝对路径：{value}"));
    }
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err(format!("拒绝盘符路径：{value}"));
    }
    let mut path = PathBuf::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!("拒绝不安全路径：{value}"));
        }
        if component.contains(':') {
            return Err(format!("拒绝备用数据流路径：{value}"));
        }
        path.push(component);
    }
    Ok(path)
}

fn is_script(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
        .unwrap_or(false)
}
