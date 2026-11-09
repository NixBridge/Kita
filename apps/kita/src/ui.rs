//! 终端展示辅助：模仿 pacman 风格的分节标题、表格与交互确认。

use std::io::{self, Write};

use kita_core::error::KitaError;

/// 打印分节标题：`:: 标题...`
pub fn header(title: &str) {
    println!(":: {title}...");
}

/// 打印缩进条目。
pub fn item(text: &str) {
    println!("   {text}");
}

/// 打印空行，用于分隔不同分节。
pub fn blank() {
    println!();
}

/// 打印带进度的条目：`(1/3) 内容`
pub fn progress(index: usize, total: usize, text: &str) {
    println!("({index}/{total}) {text}");
}

/// 打印自动列宽的表格，首尾带 `=` 分隔线。
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|header| width_of(header)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(width_of(cell));
            }
        }
    }
    let total = (widths.iter().sum::<usize>() + widths.len()).max(80);
    rule(total);
    print_row(headers, &widths);
    rule(total);
    for row in rows {
        let cells = row.iter().map(String::as_str).collect::<Vec<_>>();
        print_row(&cells, &widths);
    }
    rule(total);
}

fn rule(width: usize) {
    println!("{}", "=".repeat(width));
}

/// 估算终端显示宽度：CJK 等宽字符按 2 列计算。
fn width_of(value: &str) -> usize {
    value
        .chars()
        .map(|ch| if is_wide(ch) { 2 } else { 1 })
        .sum()
}

fn is_wide(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    )
}

fn print_row(cells: &[&str], widths: &[usize]) {
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        let width = widths
            .get(index)
            .copied()
            .unwrap_or_else(|| width_of(cell));
        line.push(' ');
        line.push_str(cell);
        line.push_str(&" ".repeat(width.saturating_sub(width_of(cell))));
    }
    println!("{}", line.trim_end());
}

/// 询问用户是否继续；空输入与 y/Y 视为同意，`--noconfirm` 时直接返回同意。
pub fn confirm(prompt: &str, noconfirm: bool) -> Result<bool, KitaError> {
    if noconfirm {
        return Ok(true);
    }
    print!("{prompt} [Y/n] ");
    io::stdout()
        .flush()
        .map_err(|error| KitaError::io("KITA-2001", "无法写入标准输出", error))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| KitaError::io("KITA-2001", "无法读取标准输入", error))?;
    let answer = answer.trim();
    Ok(answer.is_empty()
        || answer.eq_ignore_ascii_case("y")
        || answer.eq_ignore_ascii_case("yes"))
}

/// 以十进制单位格式化字节数，例如 `158.4 kB`、`45 MB`。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let number = format!("{value:.1}");
    format!("{} {}", number.trim_end_matches(".0"), UNITS[unit])
}

/// 摘要显示：取前 12 个字符。
pub fn short_digest(value: &str) -> String {
    value.chars().take(12).collect()
}
