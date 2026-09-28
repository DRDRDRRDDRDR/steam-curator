//! 数据结构：扫描产物、整理方案、合集定义。
//!
//! 所有落盘 JSON 都带 `schema` 字段，便于以后演进时识别版本。

use serde::{Deserialize, Serialize};

pub const LIBRARY_SCHEMA: &str = "steam-curator/library@1";
pub const PLAN_SCHEMA: &str = "steam-curator/plan@1";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AccountInfo {
    pub steam3_id: String,
    pub steamid64: String,
    pub persona_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RootInfo {
    pub index: String,
    pub path: String,
    pub label: String,
    pub total_size_bytes: u64,
    pub reachable: bool,
    pub game_count: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Game {
    // ---- 来自 appmanifest ----
    pub appid: u32,
    pub name: String,
    pub installdir: String,
    /// 该 appmanifest 所在的库根路径
    pub library: String,
    pub size_bytes: u64,
    /// StateFlags 第 3 位(值 4) 表示「已完整安装」
    pub state_flags: u32,
    pub installed: bool,
    pub last_updated: Option<i64>,
    pub language: Option<String>,
    pub auto_update: Option<u32>,

    // ---- 来自 userdata/<id>/config/librarycache/<appid>.json ----
    /// 官方「系列」信息（associations.rgFranchises），按系列整理时最有用的信号
    #[serde(default)]
    pub franchises: Vec<String>,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub achievements_total: u32,
    #[serde(default)]
    pub achievements_unlocked: u32,

    // ---- 来自 localconfig.vdf ----
    pub playtime_minutes: u64,
    pub playtime_2wks_minutes: u64,
    pub last_played: Option<i64>,
    pub last_played_local: Option<String>,

    // ---- 来自 cloudstorage 合集 ----
    /// 已归属的用户合集名（不含系统合集 favorite/hidden）
    pub collections: Vec<String>,
    pub hidden: bool,
    pub favorite: bool,

    // ---- 派生分析位（扫描时算好，AI 与预览共用）----
    /// 从未启动过（playtime == 0）
    pub never_launched: bool,
    pub days_since_played: Option<i64>,
    /// 已启动但总时长不足 60 分钟
    pub barely_played: bool,
    /// 超过 365 天没玩
    pub dormant: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CollectionInfo {
    pub id: String,
    pub name: String,
    pub appids: Vec<u32>,
    pub is_builtin: bool,
    pub is_dynamic: bool,
    pub is_deleted: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Library {
    pub schema: String,
    pub scanned_at: i64,
    pub scanned_at_local: String,
    pub steam_root: String,
    pub account: AccountInfo,
    pub library_roots: Vec<RootInfo>,
    pub games: Vec<Game>,
    pub existing_collections: Vec<CollectionInfo>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// 整理方案
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlanCollection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub appids: Vec<u32>,
    /// 该名字在 Steam 里已存在同名合集
    pub existing: bool,
    /// 该合集在 Steam 里已有、但方案没提到的游戏数（merge 模式下会保留）
    #[serde(default)]
    pub kept_from_existing: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Issue {
    pub level: String, // info | warn | error
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlanStats {
    pub total_games: usize,
    /// 本方案收录的游戏数
    pub assigned_games: usize,
    /// 既不在本方案、也不在现有任何合集里的游戏数
    pub unassigned_games: usize,
    /// 本方案没提、但已经被现有合集收录的游戏数
    #[serde(default)]
    pub already_filed_games: usize,
    pub collection_count: usize,
    pub new_collections: usize,
    pub updated_collections: usize,
    pub assigned_playtime_minutes: u64,
    pub assigned_size_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Plan {
    pub schema: String,
    pub created_at: i64,
    pub created_at_local: String,
    pub source: String,
    pub collections: Vec<PlanCollection>,
    pub unassigned: Vec<u32>,
    pub stats: PlanStats,
    pub issues: Vec<Issue>,
}
