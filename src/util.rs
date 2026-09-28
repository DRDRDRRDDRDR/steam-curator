//! 零依赖的小工具：时间、体积、时长、HTML 转义、文件读写。

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 本机时区相对 UTC 的偏移（分钟）。Windows 上从时区注册表读，
/// 其它平台读不到就退回 UTC。
static LOCAL_OFFSET: OnceLock<i32> = OnceLock::new();

pub fn local_offset_minutes() -> i32 {
    *LOCAL_OFFSET.get_or_init(detect_offset)
}

fn detect_offset() -> i32 {
    #[cfg(windows)]
    {
        // ActiveTimeBias 是「已含夏令时修正」的偏差，单位分钟，语义为 UTC = 本地 + bias。
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SYSTEM\CurrentControlSet\Control\TimeZoneInformation",
                "/v",
                "ActiveTimeBias",
            ])
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(pos) = text.find("0x") {
                let hex: String = text[pos + 2..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if let Ok(v) = u32::from_str_radix(&hex, 16) {
                    let bias = v as i32; // 0xfffffe20 -> -480
                    return -bias;
                }
            }
        }
    }
    0
}

/// epoch 秒 -> 本机时区的 "YYYY-MM-DD HH:MM"。
pub fn fmt_local(epoch: i64) -> String {
    let shifted = epoch + local_offset_minutes() as i64 * 60;
    let days = shifted.div_euclid(86_400);
    let secs = shifted.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// epoch 秒 -> "YYYY-MM-DD"。
pub fn fmt_date(epoch: i64) -> String {
    fmt_local(epoch).chars().take(10).collect()
}

/// Howard Hinnant 的 civil_from_days：把「1970-01-01 起的天数」转成公历年月日。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 距离今天过去了多少天；负值表示未来（时间戳异常）。
pub fn days_since(epoch: i64) -> i64 {
    (now_epoch() - epoch) / 86_400
}

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.2} {}", value, UNITS[idx])
    }
}

/// 分钟 -> "123.4 小时" / "42 分钟"。
pub fn fmt_duration(minutes: u64) -> String {
    if minutes == 0 {
        return "0 分钟".to_string();
    }
    if minutes < 60 {
        format!("{} 分钟", minutes)
    } else {
        format!("{:.1} 小时", minutes as f64 / 60.0)
    }
}

/// 分钟 -> 用于表格的紧凑写法（小时，保留一位小数）。
pub fn hours(minutes: u64) -> f64 {
    (minutes as f64 / 60.0 * 10.0).round() / 10.0
}

pub fn gb(bytes: u64) -> f64 {
    (bytes as f64 / 1_073_741_824.0 * 100.0).round() / 100.0
}

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// 写文本文件，自动创建父目录。
pub fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录 {} 失败: {}", parent.display(), e))?;
    }
    fs::write(path, content).map_err(|e| format!("写入 {} 失败: {}", path.display(), e))
}

pub fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("创建目录 {} 失败: {}", path.display(), e))
}

/// 生成一个可读、可排序、且**到秒唯一**的时间戳后缀，用于备份目录名。
pub fn stamp() -> String {
    let shifted = now_epoch() + local_offset_minutes() as i64 * 60;
    let days = shifted.div_euclid(86_400);
    let secs = shifted.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}_{:02}_{:02}_{:02}{:02}{:02}",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
