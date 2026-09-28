//! Steam `appcache/appinfo.vdf` 只读解析器 —— 从二进制 KeyValues 里取 `appid -> 游戏名`。
//!
//! 用途：给「已拥有但未安装」的游戏补名字。`scan` 只能看到 `steamapps/appmanifest_*.acf`，
//! 而 appinfo.vdf 覆盖账号可见的**全部** appid（本机实测 19,423 条），因此是补全名字的主来源。
//!
//! # 一、实测环境（本文件全部结论均出自真实字节，非文档推测）
//!
//! * 目标文件：`C:\Program Files (x86)\Steam\appcache\appinfo.vdf`，取证开始时 47,370,030 字节。
//! * 实测条目总数 **19,423**，其中成功解出 `name` 的 **19,361** 条（62 条无 `common` 表，故无名字）。
//! * 字符串表条目数 **42,435**，位于偏移 `0x02CD9018`（47,026,200）。
//! * **该文件会被运行中的 Steam 客户端就地重写**：本次会话期间实测从 47,370,030 字节变成
//!   47,370,333 字节（条目数与字符串表条数不变，35/35 比对仍全过）。所以集成方**不要缓存文件
//!   大小或偏移**，每次用之前重新读；条目数/偏移也会随后续更新变化，本文件里的数字只是
//!   「本次取证时」的基准值，不要硬编码依赖。
//!
//! # 二、文件布局
//!
//! ```text
//! [0x00]  u32   magic = 0x07564429（低字节 0x29，即社区称的 appinfo v29）
//! [0x04]  u32   universe = 1
//! [0x08]  u64   字符串表偏移（实测 0x02CD9018；指向「条目区末尾 4 字节终止符」之后）
//! [0x10]  条目区：条目1、条目2、……、u32(0) 终止符
//! [strtab] u32 字符串表条数，随后是 若干条 NUL 结尾的键名（UTF-8）
//! ```
//!
//! 条目结构（`size` 从条目数据首字节算起）：
//!
//! ```text
//! [0]  u32  appid
//! [4]  u32  size               条目数据长度（不含这 8 字节）
//! [8]  u8[size] data
//! ```
//!
//! `data` 的前 60 字节是定长头，其后是 KeyValues 块：
//!
//! ```text
//! data[ 0.. 4]  u32   infostate      实测 1 或 2
//! data[ 4.. 8]  u32   lastUpdated    Unix 时间戳
//! data[ 8..16]  u64   picsToken      实测恒为 0
//! data[16..36]  [20]  SHA1 #1        用途未定；与 KeyValues 块哈希不同
//! data[36..40]  u32   changeNumber
//! data[40..60]  [20]  SHA1 #2        = SHA1(KeyValues 块)，19,423/19,423 条实测全部匹配
//! data[60..]          KeyValues 块
//! ```
//!
//! # 三、KeyValues 块（本版本用「字符串表索引」当键，不是明文键名）
//!
//! 整块是**一个隐式根表**，内容是连续条目，直到读到最外层 `0x08` 为止：
//!
//! ```text
//! 条目 := type(1 字节) + key(4 字节 LE) + 值
//! type  : 0x00 子表（无值：key 之后直接是子表内容，由 0x08 收尾）
//!         0x01 字符串    （NUL 结尾）
//!         0x02 int32
//!         0x03 float     （4 字节）
//!         0x04 ptr       （u32）
//!         0x05 宽字符串  （UTF-16LE，0x0000 结尾）
//!         0x06 color     （u32）
//!         0x07 uint64    （8 字节）
//!         0x08 当前表结束（无 key、无值）
//! key   : u32，**字符串表下标**（0 基）
//! ```
//!
//! 游戏名路径实测为 `<根><appinfo><common><name>`，类型在 `<appinfo><common><type>`。
//! 字符串表前几个下标实测为：`0="appinfo" 1="appid" 2="public_only" 3="common" 4="name" 5="type"`。
//!
//! # 四、被证实 / 被推翻的假设
//!
//! * **被推翻**：「键名是 CRC32 哈希」。实测 `crc32("name") = 0x5E237E06`，而 `name` 的键值是
//!   `4`；`crc32("appid") = 0xA35E1483`，而 `appid` 的键值是 `1`。键值随字符串表连续递增
//!   （1、2、3、4、5…），且都能在表里按位取到正确名字 —— 它是**下标**，不是哈希。
//!   「文件尾部有一张字符串表把键还原」这一半是对的，只是还原方式是查下标。
//! * **被推翻**：条目头 40 字节。按 40 字节取 KeyValues 起点会落在 SHA1 #2 的尾部，解析立即失败；
//!   60 字节才是正确起点（并对全部 19,423 条实现了「精确消费」校验）。
//! * **被证实**：0x00 开子表、0x08 收尾、0x01 字符串、0x02 int32、0x07 uint64 等类型约定成立；
//!   全集 19,423 条用该类型表走完无一处未知类型。
//! * 键索引是否等价于 CRC32 的反证命令见 `steam-curator/work/appinfo_verify.py`。
//!
//! # 五、健壮性
//!
//! 全程不 `panic`：所有读取都做边界检查，错误一律是中文可读信息。单条想要的条目解析失败会被
//! 跳过（计入 [`ParseStats::kv_errors`]），不会让整份文件报废。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// appinfo.vdf 魔数（小端 `29 44 56 07`）。
pub const MAGIC: u32 = 0x0756_4429;

/// 条目数据里 KeyValues 块之前的定长头长度（实测，见模块文档）。
pub const ENTRY_HEADER_LEN: usize = 60;

/// 条目区起始偏移（magic 4 + universe 4 + 字符串表偏移 8）。
pub const FIRST_ENTRY_OFFSET: usize = 16;

/// 一个 app 的名字与类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub appid: u32,
    pub name: String,
    pub app_type: Option<String>,
}

/// 文件头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub magic: u32,
    pub universe: u32,
    pub string_table_offset: u64,
}

/// 解析统计，供集成方判断覆盖率，也供测试断言。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// 条目区里的条目总数（不管是否在 `wanted` 里）。
    pub total_entries: usize,
    /// `wanted` 中实际出现在文件里的条目数。
    pub wanted_entries: usize,
    /// 成功解出名字的条目数（= 返回的 map 长度）。
    pub resolved_names: usize,
    /// 字符串表条目数。
    pub string_table_entries: usize,
    /// 想要的条目里 KeyValues 解析失败的条数。
    pub kv_errors: usize,
}

// ---------------------------------------------------------------- KeyValues 类型字节

const T_TABLE: u8 = 0x00;
const T_STRING: u8 = 0x01;
const T_INT32: u8 = 0x02;
const T_FLOAT: u8 = 0x03;
const T_PTR: u8 = 0x04;
const T_WSTRING: u8 = 0x05;
const T_COLOR: u8 = 0x06;
const T_UINT64: u8 = 0x07;
const T_END: u8 = 0x08;

/// 嵌套深度上限，防止损坏/恶意数据把递归撑爆栈。
const MAX_DEPTH: u32 = 64;

// ---------------------------------------------------------------- 小工具

fn rd_u32(buf: &[u8], off: usize) -> Result<u32, String> {
    let end = off
        .checked_add(4)
        .ok_or_else(|| format!("偏移 {} 溢出", off))?;
    let s = buf
        .get(off..end)
        .ok_or_else(|| format!("读取 u32 越界：偏移 {} 超出 {} 字节的缓冲区", off, buf.len()))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 从 `off` 起找 NUL，返回 NUL 的下标。
fn find_nul(buf: &[u8], off: usize) -> Option<usize> {
    buf.get(off..)?.iter().position(|&b| b == 0).map(|i| off + i)
}

// ---------------------------------------------------------------- 头部 / 字符串表

/// 解析 16 字节文件头。
pub fn parse_header(bytes: &[u8]) -> Result<Header, String> {
    let magic = rd_u32(bytes, 0).map_err(|_| {
        format!(
            "文件太小：至少需要 16 字节才能读出 appinfo 头部，实际 {} 字节",
            bytes.len()
        )
    })?;
    if magic != MAGIC {
        return Err(format!(
            "appinfo.vdf 魔数不匹配：期望 0x{:08X}，实际 0x{:08X}（可能不是 appinfo.vdf，或 Valve 改了格式版本）",
            MAGIC, magic
        ));
    }
    let universe = rd_u32(bytes, 4)?;
    let off = rd_u32(bytes, 8)? as u64 | ((rd_u32(bytes, 12)? as u64) << 32);
    Ok(Header {
        magic,
        universe,
        string_table_offset: off,
    })
}

/// 解析文件尾部的字符串表，返回每条键名在文件中的 `(起, 止)` 字节区间（不含 NUL）。
fn parse_string_table(bytes: &[u8], off: u64) -> Result<Vec<(usize, usize)>, String> {
    let off = usize::try_from(off)
        .map_err(|_| format!("字符串表偏移 {} 超出本机可用地址空间", off))?;
    if off >= bytes.len() {
        return Err(format!(
            "字符串表偏移 {} 越界（文件共 {} 字节）",
            off,
            bytes.len()
        ));
    }
    let count = rd_u32(bytes, off)
        .map_err(|e| format!("读取字符串表条数失败（偏移 {}）：{}", off, e))?;
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(count.min(1 << 20) as usize);
    let mut p = off + 4;
    for i in 0..count {
        let end = find_nul(bytes, p).ok_or_else(|| {
            format!(
                "字符串表第 {} 条（偏移 {}）缺少 NUL 终止符，条数字段 {} 可能已损坏",
                i, p, count
            )
        })?;
        out.push((p, end));
        p = end + 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------- 关心的键名下标

struct Keys {
    appinfo: u32,
    common: u32,
    name: u32,
    type_: u32,
}

const WANTED_KEYS: [&[u8]; 4] = [b"appinfo", b"common", b"name", b"type"];

impl Keys {
    fn locate(bytes: &[u8], strtab: &[(usize, usize)]) -> Result<Keys, String> {
        let mut idx: [Option<u32>; 4] = [None; 4];
        for (i, &(s, e)) in strtab.iter().enumerate() {
            let name = match bytes.get(s..e) {
                Some(v) => v,
                None => continue,
            };
            for (slot, want) in WANTED_KEYS.iter().enumerate() {
                if idx[slot].is_none() && name == *want {
                    idx[slot] = Some(i as u32);
                }
            }
            if idx.iter().all(|v| v.is_some()) {
                break;
            }
        }
        let miss: Vec<String> = WANTED_KEYS
            .iter()
            .zip(idx.iter())
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect();
        if !miss.is_empty() {
            return Err(format!(
                "字符串表里找不到必需键名：{}（共 {} 条键名，可能不是 v29 格式）",
                miss.join("、"),
                strtab.len()
            ));
        }
        Ok(Keys {
            appinfo: idx[0].unwrap_or(0),
            common: idx[1].unwrap_or(0),
            name: idx[2].unwrap_or(0),
            type_: idx[3].unwrap_or(0),
        })
    }
}

// ---------------------------------------------------------------- KeyValues 游标

struct Kv<'a> {
    /// 整个文件（键名要在字符串表里取，落在文件缓冲区上）。
    file: &'a [u8],
    strtab: &'a [(usize, usize)],
    /// 当前条目的 KeyValues 块。
    buf: &'a [u8],
    pos: usize,
}

#[derive(Default)]
struct Found {
    name: Option<String>,
    app_type: Option<String>,
}

impl<'a> Kv<'a> {
    fn new(file: &'a [u8], strtab: &'a [(usize, usize)], buf: &'a [u8]) -> Self {
        Kv {
            file,
            strtab,
            buf,
            pos: 0,
        }
    }

    fn read_type(&mut self) -> Result<u8, String> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| format!("KeyValues 块在偏移 {} 处意外结束", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let v = rd_u32(self.buf, self.pos)
            .map_err(|e| format!("KeyValues 键/值读取失败：{}", e))?;
        self.pos += 4;
        Ok(v)
    }

    /// 读键下标并取回键名字节。
    fn read_key(&mut self) -> Result<&'a [u8], String> {
        let idx = self.read_u32()? as usize;
        let &(s, e) = self.strtab.get(idx).ok_or_else(|| {
            format!(
                "键下标 {} 超出字符串表范围（共 {} 条）",
                idx,
                self.strtab.len()
            )
        })?;
        self.file
            .get(s..e)
            .ok_or_else(|| format!("键名区间 {}..{} 超出文件范围", s, e))
    }

    fn read_cstr(&mut self) -> Result<String, String> {
        let end = find_nul(self.buf, self.pos)
            .ok_or_else(|| format!("字符串值（偏移 {}）缺少 NUL 终止符", self.pos))?;
        let raw = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| format!("字符串值区间 {}..{} 越界", self.pos, end))?;
        self.pos = end + 1;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    fn read_wstr(&mut self) -> Result<String, String> {
        let start = self.pos;
        let mut i = start;
        while i + 1 < self.buf.len() {
            if self.buf[i] == 0 && self.buf[i + 1] == 0 {
                let raw = self
                    .buf
                    .get(start..i)
                    .ok_or_else(|| format!("宽字符串区间 {}..{} 越界", start, i))?;
                let units: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                self.pos = i + 2;
                return Ok(String::from_utf16_lossy(&units));
            }
            i += 2;
        }
        Err(format!("宽字符串（偏移 {}）缺少 0x0000 终止符", start))
    }

    fn read_string_value(&mut self, t: u8) -> Result<String, String> {
        match t {
            T_STRING => self.read_cstr(),
            T_WSTRING => self.read_wstr(),
            other => Err(format!("期望字符串类型，实际是 0x{:02X}", other)),
        }
    }

    fn skip_value(&mut self, t: u8) -> Result<(), String> {
        let n = match t {
            T_STRING => return self.read_cstr().map(|_| ()),
            T_WSTRING => return self.read_wstr().map(|_| ()),
            T_INT32 | T_FLOAT | T_PTR | T_COLOR => 4,
            T_UINT64 => 8,
            other => return Err(format!("未知的 KeyValues 类型字节 0x{:02X}", other)),
        };
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| "KeyValues 值偏移溢出".to_string())?;
        if end > self.buf.len() {
            return Err(format!(
                "KeyValues 值（类型 0x{:02X}）越过块尾：需要到 {}，块长 {}",
                t,
                end,
                self.buf.len()
            ));
        }
        self.pos = end;
        Ok(())
    }

    /// 跳过一张子表（进入时游标已在表内容起点）。
    fn skip_table(&mut self, depth: u32) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(format!("KeyValues 嵌套超过 {} 层，疑似数据损坏", MAX_DEPTH));
        }
        loop {
            let t = self.read_type()?;
            if t == T_END {
                return Ok(());
            }
            let _ = self.read_key()?;
            if t == T_TABLE {
                self.skip_table(depth + 1)?;
            } else {
                self.skip_value(t)?;
            }
        }
    }

    /// 读 `common` 表的直接子项，抓 `name` / `type`。
    fn read_common(&mut self, out: &mut Found) -> Result<(), String> {
        loop {
            let t = self.read_type()?;
            if t == T_END {
                return Ok(());
            }
            let key = self.read_key()?;
            if t == T_TABLE {
                self.skip_table(1)?;
            } else if key == b"name" || key == b"type" {
                let v = self.read_string_value(t)?;
                if key == b"name" {
                    out.name.get_or_insert(v);
                } else {
                    out.app_type.get_or_insert(v);
                }
            } else {
                self.skip_value(t)?;
            }
        }
    }

    /// 在子表里找直接子表 `common`；找到就读，没找到就整表跳过。
    fn scan_for_common(&mut self, out: &mut Found) -> Result<(), String> {
        loop {
            let t = self.read_type()?;
            if t == T_END {
                return Ok(());
            }
            let key = self.read_key()?;
            if t == T_TABLE {
                if key == b"common" {
                    self.read_common(out)?;
                } else {
                    self.skip_table(1)?;
                }
            } else {
                self.skip_value(t)?;
            }
        }
    }
}

/// 定位条目数据里 KeyValues 块的起点。
///
/// 首选实测的固定偏移 60；只有当它不符合「`0x00` + 键下标 == appinfo」这一指纹时才退化为
/// 在一小段范围内扫描，避免一次格式微调就让整份文件解析不出来。
fn locate_kv_start(data: &[u8], appinfo_idx: u32) -> Option<usize> {
    fn fingerprint(data: &[u8], off: usize, appinfo_idx: u32) -> bool {
        data.get(off) == Some(&T_TABLE) && rd_u32(data, off + 1).ok() == Some(appinfo_idx)
    }
    if fingerprint(data, ENTRY_HEADER_LEN, appinfo_idx) {
        return Some(ENTRY_HEADER_LEN);
    }
    (32..96usize).find(|&o| o != ENTRY_HEADER_LEN && fingerprint(data, o, appinfo_idx))
}

/// 从一整条条目的 KeyValues 块里取出名字与类型。
fn extract_names(kv: &[u8], file: &[u8], strtab: &[(usize, usize)]) -> Result<Found, String> {
    let mut p = Kv::new(file, strtab, kv);
    let mut found = Found::default();
    loop {
        let t = p.read_type()?;
        if t == T_END {
            break;
        }
        let key = p.read_key()?;
        if t == T_TABLE {
            if key == b"common" {
                p.read_common(&mut found)?;
            } else {
                let mut sub = Found::default();
                p.scan_for_common(&mut sub)?;
                if found.name.is_none() {
                    found.name = sub.name;
                }
                if found.app_type.is_none() {
                    found.app_type = sub.app_type;
                }
            }
        } else {
            p.skip_value(t)?;
        }
    }
    if p.pos != kv.len() {
        return Err(format!(
            "KeyValues 块未精确消费：走完 {} 字节，块长 {} 字节",
            p.pos,
            kv.len()
        ));
    }
    Ok(found)
}

// ---------------------------------------------------------------- 对外 API

/// 解析 `appinfo.vdf`，返回 `wanted` 里那些 appid 的名字。
///
/// * `wanted` 之外的条目只做「读 8 字节头并跳过」，不做任何分配，因此 19,423 条也只按需解析。
/// * 失败返回中文可读错误；永不 panic。
pub fn parse_names(
    path: &Path,
    wanted: &HashSet<u32>,
) -> Result<HashMap<u32, AppEntry>, String> {
    parse_names_with_stats(path, wanted).map(|(m, _)| m)
}

/// 同 [`parse_names`]，但额外返回统计信息。
pub fn parse_names_with_stats(
    path: &Path,
    wanted: &HashSet<u32>,
) -> Result<(HashMap<u32, AppEntry>, ParseStats), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取 {} 失败：{}", path.display(), e))?;
    parse_names_from_bytes(&bytes, wanted).map_err(|e| format!("{}：{}", path.display(), e))
}

/// 走一遍条目区（不做 KeyValues 解析），返回 `(appid, 数据起, 数据止)`。
///
/// 长度越界、缺终止符等结构性问题在这里一次性报错，是 [`parse_names_from_bytes`] 与
/// [`list_appids`] 共用的边界校验。
fn walk_entries(bytes: &[u8]) -> Result<Vec<(u32, usize, usize)>, String> {
    let mut out = Vec::new();
    let mut pos = FIRST_ENTRY_OFFSET;
    loop {
        let appid = rd_u32(bytes, pos).map_err(|_| {
            format!(
                "条目区在偏移 {} 处意外结束（缺少 u32 结束标记，文件可能被截断）",
                pos
            )
        })?;
        let size = rd_u32(bytes, pos + 4)
            .map_err(|e| format!("条目头（偏移 {}）读取失败：{}", pos, e))?;
        if appid == 0 {
            break;
        }
        let data_start = pos + 8;
        let data_end = data_start
            .checked_add(size as usize)
            .ok_or_else(|| format!("appid {} 的长度字段 {} 溢出", appid, size))?;
        if data_end > bytes.len() {
            return Err(format!(
                "appid {} 声明数据长度 {} 字节，写到偏移 {}，超出文件长度 {}",
                appid,
                size,
                data_end,
                bytes.len()
            ));
        }
        out.push((appid, data_start, data_end));
        pos = data_end;
    }
    Ok(out)
}

/// 只扫描条目区，列出文件里出现的全部 appid（不解析 KeyValues，很快）。
pub fn list_appids(bytes: &[u8]) -> Result<Vec<u32>, String> {
    parse_header(bytes)?;
    Ok(walk_entries(bytes)?.into_iter().map(|(a, _, _)| a).collect())
}

/// 纯内存版本，便于测试与「已经拿到字节」的调用方。
pub fn parse_names_from_bytes(
    bytes: &[u8],
    wanted: &HashSet<u32>,
) -> Result<(HashMap<u32, AppEntry>, ParseStats), String> {
    let header = parse_header(bytes)?;
    let strtab = parse_string_table(bytes, header.string_table_offset)?;
    let keys = Keys::locate(bytes, &strtab)?;
    let entries = walk_entries(bytes)?;

    let mut stats = ParseStats {
        string_table_entries: strtab.len(),
        total_entries: entries.len(),
        ..Default::default()
    };
    let mut out: HashMap<u32, AppEntry> = HashMap::new();

    for &(appid, data_start, data_end) in &entries {
        if !wanted.contains(&appid) {
            continue;
        }
        stats.wanted_entries += 1;
        let data = &bytes[data_start..data_end];
        let parsed = locate_kv_start(data, keys.appinfo)
            .ok_or_else(|| "定位 KeyValues 块起点失败".to_string())
            .and_then(|off| extract_names(&data[off..], bytes, &strtab));
        match parsed {
            Ok(found) => {
                if let Some(name) = found.name {
                    out.insert(
                        appid,
                        AppEntry {
                            appid,
                            name,
                            app_type: found.app_type,
                        },
                    );
                }
            }
            // 单条坏条目不影响整份文件：计数后跳过。
            Err(_e) => stats.kv_errors += 1,
        }
    }

    stats.resolved_names = out.len();
    Ok((out, stats))
}

/// 猜一个本机 appinfo.vdf 路径（存在才返回）。
pub fn default_appinfo_path() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("STEAM_CURATOR_APPINFO") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        cands.push(PathBuf::from(&pf86).join("Steam/appcache/appinfo.vdf"));
    }
    cands.push(PathBuf::from(
        r"C:\Program Files (x86)\Steam\appcache\appinfo.vdf",
    ));
    if let Ok(pf) = std::env::var("ProgramFiles") {
        cands.push(PathBuf::from(pf).join("Steam/appcache/appinfo.vdf"));
    }
    cands.into_iter().find(|p| p.is_file())
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    // ---- (a) 头部解析 ----

    #[test]
    fn parses_real_header_bytes() {
        // 实测文件的前 16 字节。
        let raw = [
            0x29, 0x44, 0x56, 0x07, // magic
            0x01, 0x00, 0x00, 0x00, // universe = 1
            0x18, 0x90, 0xcd, 0x02, 0x00, 0x00, 0x00, 0x00, // strtab = 0x02CD9018
        ];
        let h = parse_header(&raw).expect("头部应解析成功");
        assert_eq!(h.magic, MAGIC);
        assert_eq!(h.magic, 0x0756_4429);
        assert_eq!(h.universe, 1);
        assert_eq!(h.string_table_offset, 0x02CD_9018);
        assert_eq!(h.string_table_offset, 47_026_200);
    }

    #[test]
    fn rejects_bad_magic_and_short_file() {
        let mut raw = [0u8; 16];
        raw[0] = 0x28; // 只差 1 的魔数
        let e = parse_header(&raw).unwrap_err();
        assert!(e.contains("魔数不匹配"), "错误信息应可读：{}", e);

        // 只有 2 字节：连 magic 都读不出来
        let e = parse_header(&[0x29u8, 0x44]).unwrap_err();
        assert!(e.contains("文件太小"), "错误信息应可读：{}", e);
    }

    // ---- (c) 合成字节流（不依赖本机是否装了 Steam） ----

    fn push_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    /// 造一份最小可解析的 appinfo.vdf：
    /// 字符串表 ["appinfo","appid","common","name","type"]，
    /// 条目 appid=42（name="Test Game" type="Game"）与 appid=99（name="Other"）。
    fn synthetic() -> Vec<u8> {
        // 条目 1 的 KeyValues 块
        let mut kv = Vec::new();
        kv.push(T_TABLE); // 根 → appinfo 子表
        push_u32(&mut kv, 0);
        kv.push(T_INT32); // appinfo.appid = 42
        push_u32(&mut kv, 1);
        push_u32(&mut kv, 42);
        kv.push(T_TABLE); // appinfo.common
        push_u32(&mut kv, 2);
        kv.push(T_STRING); // common.name = "Test Game"
        push_u32(&mut kv, 3);
        kv.extend_from_slice(b"Test Game\0");
        kv.push(T_STRING); // common.type = "Game"
        push_u32(&mut kv, 4);
        kv.extend_from_slice(b"Game\0");
        kv.push(T_FLOAT); // common.unused = 1.5（用来证明非字符串值会被正确跳过）
        push_u32(&mut kv, 5);
        kv.extend_from_slice(&1.5f32.to_le_bytes());
        kv.push(T_END); // 收 common
        kv.push(T_END); // 收 appinfo
        kv.push(T_END); // 收根

        // 条目 2 的 KeyValues 块（appid=99，仍带名字，用于验证 wanted 过滤）
        let mut kv2 = Vec::new();
        kv2.push(T_TABLE);
        push_u32(&mut kv2, 0);
        kv2.push(T_TABLE);
        push_u32(&mut kv2, 2);
        kv2.push(T_STRING);
        push_u32(&mut kv2, 3);
        kv2.extend_from_slice(b"Other\0");
        kv2.push(T_END);
        kv2.push(T_END);
        kv2.push(T_END);

        let mut bodies: Vec<Vec<u8>> = Vec::new();
        for kv in [kv, kv2] {
            let mut d = Vec::new();
            push_u32(&mut d, 1); // infostate
            push_u32(&mut d, 0x6a396cae); // lastUpdated
            d.extend_from_slice(&[0u8; 8]); // picsToken
            d.extend_from_slice(&[0xAAu8; 20]); // SHA1 #1（本解析器不校验）
            push_u32(&mut d, 7); // changeNumber
            d.extend_from_slice(&[0xBBu8; 20]); // SHA1 #2（本解析器不校验）
            assert_eq!(d.len(), ENTRY_HEADER_LEN);
            d.extend_from_slice(&kv);
            bodies.push(d);
        }

        // 字符串表
        let mut strtab = Vec::new();
        push_u32(&mut strtab, 6);
        for s in ["appinfo", "appid", "common", "name", "type", "unused"] {
            strtab.extend_from_slice(s.as_bytes());
            strtab.push(0);
        }

        // 文件头 + 条目区 + 终止符 + 字符串表
        let mut out = Vec::new();
        push_u32(&mut out, MAGIC);
        push_u32(&mut out, 1);
        let strtab_off = (FIRST_ENTRY_OFFSET
            + bodies.iter().map(|b| 8 + b.len()).sum::<usize>()
            + 4) as u64;
        out.extend_from_slice(&strtab_off.to_le_bytes());
        for (appid, body) in [42u32, 99u32].iter().zip(bodies.iter()) {
            push_u32(&mut out, *appid);
            push_u32(&mut out, body.len() as u32);
            out.extend_from_slice(body);
        }
        push_u32(&mut out, 0); // 条目区终止符
        assert_eq!(out.len() as u64, strtab_off);
        out.extend_from_slice(&strtab);
        out
    }

    #[test]
    fn parses_synthetic_stream() {
        let bytes = synthetic();
        let mut wanted = HashSet::new();
        wanted.insert(42);
        let (map, stats) = parse_names_from_bytes(&bytes, &wanted).unwrap();

        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.wanted_entries, 1);
        assert_eq!(stats.resolved_names, 1);
        assert_eq!(stats.kv_errors, 0);
        assert_eq!(stats.string_table_entries, 6);

        let e = map.get(&42).expect("应解出 appid 42");
        assert_eq!(
            e,
            &AppEntry {
                appid: 42,
                name: "Test Game".to_string(),
                app_type: Some("Game".to_string()),
            }
        );
        // wanted 之外的条目不该出现
        assert!(!map.contains_key(&99));
    }

    #[test]
    fn synthetic_unsupported_version_reports_chinese_error() {
        let mut bytes = synthetic();
        bytes[0] = 0x28;
        let e = parse_names_from_bytes(&bytes, &HashSet::new()).unwrap_err();
        assert!(e.contains("魔数不匹配"), "{}", e);
    }

    // ---- (b) 真机文件 ----

    #[test]
    fn parses_real_appinfo_file() {
        let path = match default_appinfo_path() {
            Some(p) => p,
            None => {
                eprintln!("[跳过] 本机找不到 appinfo.vdf");
                return;
            }
        };
        let bytes = std::fs::read(&path).expect("读取 appinfo.vdf");
        let (map, stats) =
            parse_names_from_bytes(&bytes, &HashSet::new()).expect("应能解析真实 appinfo.vdf");
        eprintln!(
            "[真实文件] {} 字节；条目总数 {}；字符串表 {} 条；无 wanted，解出名字 {}",
            bytes.len(),
            stats.total_entries,
            stats.string_table_entries,
            stats.resolved_names
        );
        assert!(
            stats.total_entries > 1000,
            "条目总数应当 > 1000，实际 {}",
            stats.total_entries
        );
        assert!(stats.string_table_entries > 1000);
        assert!(map.is_empty());

        // 再带上若干个真实 appid，验证过滤路径与名字解析。
        let mut wanted: HashSet<u32> = HashSet::new();
        for a in [5u32, 7, 10, 1510, 469920, 374320] {
            wanted.insert(a);
        }
        let (map, stats2) = parse_names_from_bytes(&bytes, &wanted).unwrap();
        eprintln!(
            "[真实文件] wanted {} 条 → 解出 {} 条，KV 失败 {} 条",
            stats2.wanted_entries, stats2.resolved_names, stats2.kv_errors
        );
        assert_eq!(stats2.kv_errors, 0);
        assert_eq!(map.get(&7).map(|e| e.name.as_str()), Some("Steam Client"));
        assert_eq!(map.get(&1510).map(|e| e.name.as_str()), Some("Uplink"));
    }

    /// 把文件里**每一个**条目都解析一遍：证明解析器对全集都成立，不只是挑出来的几条。
    #[test]
    fn parses_every_entry_in_real_file() {
        let path = match default_appinfo_path() {
            Some(p) => p,
            None => {
                eprintln!("[跳过] 本机找不到 appinfo.vdf");
                return;
            }
        };
        let bytes = std::fs::read(&path).expect("读取 appinfo.vdf");
        let all = list_appids(&bytes).expect("应能列出全部 appid");
        let wanted: HashSet<u32> = all.iter().copied().collect();
        assert_eq!(all.len(), wanted.len(), "条目区出现了重复 appid");

        let (map, stats) = parse_names_from_bytes(&bytes, &wanted).expect("全量解析");
        eprintln!(
            "[全量解析] 条目 {} 条；解出名字 {} 条；无名字 {} 条；KV 解析失败 {} 条",
            stats.total_entries,
            stats.resolved_names,
            stats.total_entries - stats.resolved_names,
            stats.kv_errors
        );
        assert_eq!(stats.total_entries, all.len());
        assert_eq!(stats.wanted_entries, all.len());
        assert_eq!(stats.kv_errors, 0, "全集里不该有条目解析失败");
        assert!(
            stats.resolved_names > 15_000,
            "本机实测解出名字的条目数应在一万九千条量级，实际 {}",
            stats.resolved_names
        );
        // 抽查几个众所周知的名字
        assert_eq!(map.get(&7).map(|e| e.name.as_str()), Some("Steam Client"));
        assert_eq!(map.get(&228980).map(|e| e.name.as_str()), Some("Steamworks Common Redistributables"));
        assert_eq!(
            map.get(&228980).and_then(|e| e.app_type.as_deref()),
            Some("Tool")
        );
    }

    // ---- (4) 与已安装 appmanifest 逐条比对（35/35） ----

    #[test]
    fn installed_manifest_names_match_appinfo() {
        let appinfo = match default_appinfo_path() {
            Some(p) => p,
            None => {
                eprintln!("[跳过] 本机找不到 appinfo.vdf");
                return;
            }
        };
        // appcache/appinfo.vdf -> <Steam>/steamapps
        let steamapps = match appinfo.parent().and_then(|p| p.parent()) {
            Some(root) => root.join("steamapps"),
            None => {
                eprintln!("[跳过] 无法从 {} 推出 steamapps", appinfo.display());
                return;
            }
        };
        let mut manifests: Vec<(u32, String, PathBuf)> = Vec::new();
        let rd = match std::fs::read_dir(&steamapps) {
            Ok(rd) => rd,
            Err(e) => {
                eprintln!("[跳过] 读不到 {}：{}", steamapps.display(), e);
                return;
            }
        };
        for ent in rd.flatten() {
            let p = ent.path();
            let fname = ent.file_name().to_string_lossy().into_owned();
            if !fname.starts_with("appmanifest_") || !fname.ends_with(".acf") {
                continue;
            }
            let appid: u32 = match fname
                .trim_start_matches("appmanifest_")
                .trim_end_matches(".acf")
                .parse()
            {
                Ok(v) => v,
                Err(_) => continue,
            };
            let table = match crate::vdf::parse_file(&p) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[跳过] {} 解析失败：{}", fname, e);
                    continue;
                }
            };
            if let Some(name) = table.str_ci("name") {
                manifests.push((appid, name.to_string(), p));
            }
        }
        if manifests.is_empty() {
            eprintln!("[跳过] {} 下没有 appmanifest_*.acf", steamapps.display());
            return;
        }

        let wanted: HashSet<u32> = manifests.iter().map(|(a, _, _)| *a).collect();
        let (map, stats) = parse_names_with_stats(&appinfo, &wanted).expect("解析 appinfo.vdf");
        eprintln!(
            "[35 比对] appinfo 条目总数 {}，字符串表 {} 条，wanted {} 条，解出 {} 条，KV 失败 {} 条",
            stats.total_entries,
            stats.string_table_entries,
            stats.wanted_entries,
            stats.resolved_names,
            stats.kv_errors
        );

        let mut ok = 0usize;
        let mut mismatch: Vec<String> = Vec::new();
        let mut missing: Vec<u32> = Vec::new();
        let mut rows: Vec<(u32, String, String)> = Vec::new();
        for (appid, expect, _) in &manifests {
            match map.get(appid) {
                Some(e) if &e.name == expect => {
                    ok += 1;
                    rows.push((*appid, "OK".to_string(), expect.clone()));
                }
                Some(e) => {
                    rows.push((*appid, "不一致".to_string(), e.name.clone()));
                    mismatch.push(format!(
                        "appid {}：appinfo={:?} appmanifest={:?}",
                        appid, e.name, expect
                    ));
                }
                None => {
                    rows.push((*appid, "缺失".to_string(), expect.clone()));
                    missing.push(*appid);
                }
            }
        }
        rows.sort_by_key(|r| r.0);
        for (appid, verdict, name) in &rows {
            eprintln!("[35 比对] {:>8}  {:<4}  {}", appid, verdict, name);
        }
        eprintln!(
            "[35 比对] 结果 {}/{} 一致，{} 不一致，{} 缺失",
            ok,
            manifests.len(),
            mismatch.len(),
            missing.len()
        );
        assert!(mismatch.is_empty(), "名字不一致：\n{}", mismatch.join("\n"));
        assert!(
            missing.is_empty(),
            "appinfo.vdf 里找不到这些已安装 appid：{:?}",
            missing
        );
        assert_eq!(ok, manifests.len());
    }
}
