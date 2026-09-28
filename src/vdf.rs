//! Valve KeyValues("VDF" 文本格式)解析器。
//!
//! Steam 的 `libraryfolders.vdf` / `appmanifest_*.acf` / `localconfig.vdf` 都是这个格式。
//! 实测要点（见 docs/STEAM-FORMATS.md）：
//!   * 同一个文件里键的**大小写不固定**（`appid` vs `StateFlags` vs `lastupdated`），
//!     Valve 自己也当它大小写不敏感 —— 所以这里统一用 `*_ci` 系列取值。
//!   * 值一律是带引号字符串；条目块内键的顺序不稳定，绝不能按位置解析。

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum Vdf {
    Str(String),
    Table(Table),
}

#[derive(Debug, Clone, Default)]
pub struct Table(pub BTreeMap<String, Vdf>);

impl std::ops::Deref for Table {
    type Target = BTreeMap<String, Vdf>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Table {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'a> IntoIterator for &'a Table {
    type Item = (&'a String, &'a Vdf);
    type IntoIter = std::collections::btree_map::Iter<'a, String, Vdf>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for Table {
    type Item = (String, Vdf);
    type IntoIter = std::collections::btree_map::IntoIter<String, Vdf>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Vdf {
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Vdf::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl Table {
    pub fn get_ci(&self, key: &str) -> Option<&Vdf> {
        if let Some(v) = self.get(key) {
            return Some(v);
        }
        self.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn str_ci(&self, key: &str) -> Option<&str> {
        self.get_ci(key).and_then(|v| v.as_str())
    }

    pub fn table_ci(&self, key: &str) -> Option<&Table> {
        self.get_ci(key).and_then(|v| v.as_table())
    }

    pub fn u64_ci(&self, key: &str) -> Option<u64> {
        self.str_ci(key).and_then(|s| s.trim().parse().ok())
    }

    pub fn i64_ci(&self, key: &str) -> Option<i64> {
        self.str_ci(key).and_then(|s| s.trim().parse().ok())
    }
}

/// 解析一份 VDF 文档，返回根键下的表。
pub fn parse(input: &str) -> Result<Table, String> {
    // 去掉 UTF-8 BOM
    let text = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut p = Parser {
        bytes: text.as_bytes(),
        pos: 0,
        line: 1,
    };
    p.skip_trivia();
    let _root = p.parse_string()?;
    p.skip_trivia();
    if !p.peek(b'{') {
        return Err(format!(
            "第 {} 行：根键之后应当是 '{{'，实际是 {:?}",
            p.line,
            p.snippet()
        ));
    }
    let table = p.parse_table()?;
    Ok(table)
}

/// 从磁盘读取并解析。
pub fn parse_file(path: &std::path::Path) -> Result<Table, String> {
    let raw = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    parse(&text).map_err(|e| format!("{}: {}", path.display(), e))
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn cur(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek(&self, b: u8) -> bool {
        self.cur() == Some(b)
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.cur();
        if let Some(c) = b {
            self.pos += 1;
            if c == b'\n' {
                self.line += 1;
            }
        }
        b
    }

    fn snippet(&self) -> String {
        let end = (self.pos + 24).min(self.bytes.len());
        String::from_utf8_lossy(&self.bytes[self.pos..end]).into_owned()
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.cur() {
                Some(c) if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' => {
                    self.bump();
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(c) = self.cur() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    /// 读取一个带引号字符串；为容错也接受裸 token（无引号）。
    fn parse_string(&mut self) -> Result<String, String> {
        if self.eof() {
            return Err(format!("第 {} 行：文件意外结束", self.line));
        }
        if !self.peek(b'"') {
            // 裸 token
            let start = self.pos;
            while let Some(c) = self.cur() {
                if c.is_ascii_whitespace() || c == b'{' || c == b'}' || c == b'"' {
                    break;
                }
                self.bump();
            }
            if self.pos == start {
                return Err(format!("第 {} 行：期望字符串，实际是 {:?}", self.line, self.snippet()));
            }
            return Ok(String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned());
        }

        self.bump(); // 开引号
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match self.bump() {
                None => return Err(format!("第 {} 行：字符串未闭合", self.line)),
                Some(b'"') => break,
                Some(b'\\') => {
                    let esc = self.bump().ok_or_else(|| {
                        format!("第 {} 行：转义符后文件结束", self.line)
                    })?;
                    match esc {
                        b'n' => buf.push(b'\n'),
                        b't' => buf.push(b'\t'),
                        b'r' => buf.push(b'\r'),
                        b'\\' => buf.push(b'\\'),
                        b'"' => buf.push(b'"'),
                        other => buf.push(other),
                    }
                }
                Some(c) => buf.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn parse_table(&mut self) -> Result<Table, String> {
        debug_assert!(self.peek(b'{'));
        self.bump(); // '{'
        let mut map = Table::default();
        loop {
            self.skip_trivia();
            match self.cur() {
                None => return Err(format!("第 {} 行：表未闭合（缺少 '}}'）", self.line)),
                Some(b'}') => {
                    self.bump();
                    return Ok(map);
                }
                Some(_) => {
                    let key = self.parse_string()?;
                    self.skip_trivia();
                    if self.peek(b'{') {
                        let nested = self.parse_table()?;
                        map.insert(key, Vdf::Table(nested));
                    } else {
                        let value = self.parse_string()?;
                        map.insert(key, Vdf::Str(value));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_tables_and_escapes() {
        let src = r#"
"root"
{
    // 注释
    "path"    "C:\\Program Files (x86)\\Steam"
    "0"
    {
        "appid"  "1510"
        "name"   "Test \"Game\""
    }
}
"#;
        let root = parse(src).unwrap();
        assert_eq!(root.str_ci("path").unwrap(), r"C:\Program Files (x86)\Steam");
        let child = root.table_ci("0").unwrap();
        assert_eq!(child.u64_ci("appid").unwrap(), 1510);
        assert_eq!(child.str_ci("NAME").unwrap(), "Test \"Game\"");
    }
}
