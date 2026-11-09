use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{KitaError, Result};

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| {
        KitaError::io(
            "KITA-2001",
            format!("无法创建目录 {}", path.display()),
            error,
        )
    })
}

pub fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

/// Write a file by creating a sibling temporary file, flushing it, and then
/// renaming it into place. Existing files are moved to `<name>.bak` first.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| KitaError::new("KITA-2001", "目标文件没有父目录"))?;
    ensure_dir(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("kita.tmp");
    let temp = parent.join(format!(".{file_name}.{}.tmp", unique_suffix()));

    {
        let mut file = File::create(&temp).map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法创建临时文件 {}", temp.display()),
                error,
            )
        })?;
        file.write_all(bytes).map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法写入临时文件 {}", temp.display()),
                error,
            )
        })?;
        file.sync_all().map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法刷新临时文件 {}", temp.display()),
                error,
            )
        })?;
    }

    if path.exists() {
        let backup = backup_path(path);
        let _ = fs::remove_file(&backup);
        fs::rename(path, &backup).map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法备份 {}", path.display()),
                error,
            )
        })?;
        if let Err(error) = fs::rename(&temp, path) {
            let _ = fs::rename(&backup, path);
            let _ = fs::remove_file(&temp);
            return Err(KitaError::io(
                "KITA-2001",
                format!("无法替换 {}", path.display()),
                error,
            ));
        }
    } else {
        fs::rename(&temp, path).map_err(|error| {
            KitaError::io(
                "KITA-2001",
                format!("无法写入 {}", path.display()),
                error,
            )
        })?;
    }
    Ok(())
}

pub fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("kita");
    path.with_file_name(format!("{name}.bak"))
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| KitaError::new("KITA-2001", format!("JSON 序列化失败：{error}")))?;
    atomic_write(path, &bytes)
}

pub fn read_json_opt<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(KitaError::io(
                "KITA-2001",
                format!("无法读取 {}", path.display()),
                error,
            ))
        }
    };
    let mut bytes = Vec::new();
    let mut reader = std::io::BufReader::new(file);
    reader.read_to_end(&mut bytes).map_err(|error| {
        KitaError::io(
            "KITA-2001",
            format!("无法读取 {}", path.display()),
            error,
        )
    })?;
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        KitaError::new(
            "KITA-2001",
            format!("{} 解析失败：{error}", path.display()),
        )
        .with_hint("运行 `kita doctor` 检查本地状态")
    })?;
    Ok(Some(value))
}

pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KitaError::io(
            "KITA-2001",
            format!("无法删除 {}", path.display()),
            error,
        )),
    }
}

pub fn slug(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub fn is_valid_command_name(name: &str) -> bool {
    if name.len() > 64 {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn is_script_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
        .unwrap_or(false)
}

/// Expands the restricted `${NAME}` template vocabulary used by package env.
pub fn expand_template(template: &str, variables: &[(&str, &str)]) -> Result<String> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            KitaError::new(
                "KITA-1003",
                format!("环境变量模板未闭合：{template}"),
            )
        })?;
        let name = &after[..end];
        let value = variables
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
            .ok_or_else(|| {
                KitaError::new(
                    "KITA-1003",
                    format!("环境变量模板包含未知变量 `${{{name}}}`"),
                )
            })?;
        output.push_str(value);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

pub fn current_target() -> Result<String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64-pc-windows-msvc".to_string()),
        "aarch64" => Ok("aarch64-pc-windows-msvc".to_string()),
        other => Err(KitaError::new(
            "KITA-9002",
            format!("Kita 当前不支持架构 `{other}`"),
        )),
    }
}

pub fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

pub fn default_root() -> PathBuf {
    if let Some(path) = std::env::var_os("KITA_ROOT") {
        return PathBuf::from(path);
    }
    if let Some(base) = local_app_data() {
        return base.join("Kita");
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile).join(".kita");
    }
    PathBuf::from(".kita")
}

/// Cooperative state lock. This is a simple exclusive lock file, not OS-level
/// advisory locking: a crashed process leaves the file behind and the user is
/// told how to recover.
pub struct StateLock {
    path: PathBuf,
}

impl StateLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let directory = root.join("state");
        ensure_dir(&directory)?;
        let path = directory.join("lock");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(Self { path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(
                KitaError::new(
                    "KITA-2001",
                    format!("状态锁已被占用：{}", path.display()),
                )
                .with_hint("如果确认没有其他 kita 进程在运行，可删除该锁文件后重试"),
            ),
            Err(error) => Err(KitaError::io(
                "KITA-2001",
                format!("无法创建状态锁 {}", path.display()),
                error,
            )),
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{expand_template, is_valid_command_name, slug};

    #[test]
    fn expands_known_variables() {
        assert_eq!(
            expand_template("${KITA_DATA}/x", &[("KITA_DATA", "C:\\data")]).unwrap(),
            "C:\\data/x"
        );
        assert!(expand_template("${UNKNOWN}", &[]).is_err());
    }

    #[test]
    fn validates_command_names() {
        assert!(is_valid_command_name("rg"));
        assert!(is_valid_command_name("ast-grep"));
        assert!(!is_valid_command_name("Rg"));
        assert!(!is_valid_command_name("../rg"));
    }

    #[test]
    fn slugs_identifiers() {
        assert_eq!(slug("kita/ripgrep"), "kita_ripgrep");
        assert_eq!(slug("14.1.1"), "14.1.1");
    }
}
