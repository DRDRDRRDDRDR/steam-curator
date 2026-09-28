//! `preview`：生成一份**自包含**的 HTML 预览报告。
//!
//! 刻意不引入任何外部资源（无 CDN、无图片、无字体），双击即看、断网也能看。
//! 图表用纯 CSS 画，表格搜索用一小段原生 JS。

use crate::apply::ApplyOutcome;
use crate::model::{Game, Library, Plan};
use crate::scan;
use crate::util;

pub struct PreviewInput<'a> {
    pub library: &'a Library,
    pub plan: Option<&'a Plan>,
    pub outcome: Option<&'a ApplyOutcome>,
    pub title: String,
}

pub fn build(input: PreviewInput) -> String {
    let lib = input.library;
    let s = scan::summarize(lib);
    let mut o = String::with_capacity(256 * 1024);

    o.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    o.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    o.push_str(&format!(
        "<title>{}</title>\n",
        util::escape_html(&input.title)
    ));
    o.push_str(STYLE);
    o.push_str("</head>\n<body>\n");

    // ---------- 头部 ----------
    o.push_str("<header>\n");
    o.push_str(&format!(
        "<h1>{}</h1>\n",
        util::escape_html(&input.title)
    ));
    let who = lib
        .account
        .persona_name
        .clone()
        .unwrap_or_else(|| lib.account.steam3_id.clone());
    o.push_str(&format!(
        "<p class=\"sub\">账号 <b>{}</b>（{}） · 库 <code>{}</code> · 扫描于 {}</p>\n",
        util::escape_html(&who),
        util::escape_html(&lib.account.steam3_id),
        util::escape_html(&lib.steam_root),
        util::escape_html(&lib.scanned_at_local)
    ));
    o.push_str("</header>\n<main>\n");

    // ---------- 统计卡 ----------
    o.push_str("<section><h2>库概览</h2><div class=\"cards\">\n");
    card(&mut o, &s.games.to_string(), "已安装游戏", "");
    card(&mut o, &util::fmt_size(s.total_size), "占用磁盘", "");
    card(
        &mut o,
        &util::fmt_duration(s.total_playtime_minutes),
        "累计游玩",
        "",
    );
    card(
        &mut o,
        &s.never_launched.to_string(),
        "从未启动",
        if s.never_launched > 0 { "warn" } else { "" },
    );
    card(&mut o, &s.dormant.to_string(), "一年以上没玩", "");
    card(
        &mut o,
        &s.unassigned.to_string(),
        "尚未归入合集",
        if s.unassigned > 0 { "warn" } else { "ok" },
    );
    o.push_str("</div></section>\n");

    // ---------- 变更预览（apply 结果）----------
    if let Some(oc) = input.outcome {
        o.push_str("<section><h2>写回预览");
        if oc.dry_run {
            o.push_str(" <span class=\"pill dry\">演练模式 · 未改动任何文件</span>");
        } else {
            o.push_str(" <span class=\"pill written\">已写入</span>");
        }
        o.push_str("</h2>\n");
        if let Some(reason) = &oc.blocked_reason {
            o.push_str(&format!(
                "<div class=\"alert error\">{}</div>\n",
                util::escape_html(reason)
            ));
        }
        o.push_str(&format!(
            "<p class=\"sub\">目标文件 <code>{}</code><br>命名空间版本 {} → {}</p>\n",
            util::escape_html(&oc.namespace_path),
            oc.old_namespace_version,
            oc.new_namespace_version
        ));
        if let Some(b) = &oc.backup_dir {
            o.push_str(&format!(
                "<p class=\"sub\">备份目录 <code>{}</code></p>\n",
                util::escape_html(b)
            ));
        }
        o.push_str("<table class=\"tbl\"><thead><tr><th>动作</th><th>合集</th><th>最终成员</th><th>新增</th><th>保留</th><th>移除</th><th>说明</th></tr></thead><tbody>\n");
        for a in &oc.actions {
            let (kind_label, kind_class) = match a.kind.as_str() {
                "create" => ("新建", "k-create"),
                "update" => ("更新", "k-update"),
                "unchanged" => ("无变化", "k-unchanged"),
                "delete" => ("删除", "k-delete"),
                _ => ("跳过", "k-skip"),
            };
            o.push_str(&format!(
                "<tr><td><span class=\"k {}\">{}</span></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"dim\">{}</td></tr>\n",
                kind_class,
                kind_label,
                util::escape_html(&a.name),
                a.added.len(),
                if a.added_delta.is_empty() { "—".to_string() } else { format!("+{}", a.added_delta.len()) },
                if a.kept_from_existing.is_empty() { "—".to_string() } else { format!("{}", a.kept_from_existing.len()) },
                if a.dropped.is_empty() { "—".to_string() } else { format!("-{}", a.dropped.len()) },
                util::escape_html(a.note.as_deref().unwrap_or(""))
            ));
        }
        o.push_str("</tbody></table></section>\n");
    }

    // ---------- 整理方案 ----------
    if let Some(plan) = input.plan {
        o.push_str("<section><h2>整理方案");
        o.push_str(&format!(
            " <span class=\"pill\">{} 个合集 · 覆盖 {} / {} 款游戏</span>",
            plan.collections.len(),
            plan.stats.assigned_games,
            plan.stats.total_games
        ));
        o.push_str("</h2>\n");

        // 问题清单
        if !plan.issues.is_empty() {
            o.push_str("<h3>校验结果</h3>\n<div class=\"issues\">\n");
            for i in &plan.issues {
                let cls = match i.level.as_str() {
                    "error" => "error",
                    "warn" => "warn",
                    _ => "info",
                };
                o.push_str(&format!(
                    "<div class=\"alert {}\">{}</div>\n",
                    cls,
                    util::escape_html(&i.message)
                ));
            }
            o.push_str("</div>\n");
        }

        let by_id: std::collections::BTreeMap<u32, &Game> =
            lib.games.iter().map(|g| (g.appid, g)).collect();

        o.push_str("<h3>合集一览</h3>\n<div class=\"collections\">\n");
        for c in &plan.collections {
            o.push_str("<article class=\"coll\">\n<header>\n");
            o.push_str(&format!("<h4>{}</h4>\n", util::escape_html(&c.name)));
            let mut badges = vec![format!("{} 款", c.appids.len())];
            if c.existing {
                if c.kept_from_existing > 0 {
                    badges.push(format!("已有合集 · 保留原 {} 款", c.kept_from_existing));
                } else {
                    badges.push("已有合集".to_string());
                }
            } else {
                badges.push("新建".to_string());
            }
            let (mut mins, mut bytes) = (0u64, 0u64);
            for a in &c.appids {
                if let Some(g) = by_id.get(a) {
                    mins += g.playtime_minutes;
                    bytes += g.size_bytes;
                }
            }
            badges.push(util::fmt_duration(mins));
            badges.push(util::fmt_size(bytes));
            o.push_str(&format!(
                "<div class=\"badges\">{}</div>\n",
                badges
                    .iter()
                    .map(|b| format!("<span>{}</span>", util::escape_html(b)))
                    .collect::<Vec<_>>()
                    .join("")
            ));
            o.push_str("</header>\n");
            if let Some(d) = &c.description {
                if !d.trim().is_empty() {
                    o.push_str(&format!("<p class=\"desc\">{}</p>\n", util::escape_html(d)));
                }
            }
            o.push_str("<ul class=\"games\">\n");
            for a in &c.appids {
                match by_id.get(a) {
                    Some(g) => o.push_str(&format!(
                        "<li title=\"{} GB · 玩过 {} · 最后游玩 {}\"><span class=\"gname\">{}</span><span class=\"gmeta\">{:.1}h · {:.1}GB{}</span></li>\n",
                        util::gb(g.size_bytes),
                        util::fmt_duration(g.playtime_minutes),
                        g.last_played_local.as_deref().unwrap_or("从未启动"),
                        util::escape_html(&g.name),
                        util::hours(g.playtime_minutes),
                        util::gb(g.size_bytes),
                        if g.never_launched { " · <b class=\"never\">未启动</b>" } else { "" }
                    )),
                    None => o.push_str(&format!(
                        "<li class=\"unknown\"><span class=\"gname\">appid {}</span><span class=\"gmeta\">不在当前库中</span></li>\n",
                        a
                    )),
                }
            }
            o.push_str("</ul>\n</article>\n");
        }
        o.push_str("</div>\n");

        if !plan.unassigned.is_empty() {
            o.push_str(&format!(
                "<h3>未被归档（{} 款）</h3>\n<div class=\"chips\">\n",
                plan.unassigned.len()
            ));
            for a in &plan.unassigned {
                let name = by_id
                    .get(a)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| format!("appid {}", a));
                o.push_str(&format!(
                    "<span class=\"chip\">{}</span>\n",
                    util::escape_html(&name)
                ));
            }
            o.push_str("</div>\n");
        }
        o.push_str("</section>\n");
    }

    // ---------- 数据洞察 ----------
    o.push_str("<section><h2>数据洞察</h2>\n");

    o.push_str("<h3>占盘大户 Top 15</h3><div class=\"bars\">\n");
    let mut by_size: Vec<&Game> = lib.games.iter().collect();
    by_size.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    let max_size = by_size.first().map(|g| g.size_bytes).unwrap_or(1).max(1);
    for g in by_size.iter().take(15) {
        bar_row(
            &mut o,
            &g.name,
            &util::fmt_size(g.size_bytes),
            g.size_bytes as f64 / max_size as f64,            if g.never_launched { "never" } else { "size" },
        );
    }
    o.push_str("</div>\n");

    o.push_str("<h3>游玩时长 Top 15</h3><div class=\"bars\">\n");
    let mut by_time: Vec<&Game> = lib.games.iter().filter(|g| g.playtime_minutes > 0).collect();
    by_time.sort_by(|a, b| b.playtime_minutes.cmp(&a.playtime_minutes));
    let max_time = by_time
        .first()
        .map(|g| g.playtime_minutes)
        .unwrap_or(1)
        .max(1);
    for g in by_time.iter().take(15) {
        bar_row(
            &mut o,
            &g.name,
            &util::fmt_duration(g.playtime_minutes),
            g.playtime_minutes as f64 / max_time as f64,
            "time",
        );
    }
    o.push_str("</div>\n");

    // 吃灰 / 未启动
    let mut idle: Vec<&Game> = lib
        .games
        .iter()
        .filter(|g| g.never_launched || g.dormant)
        .collect();
    idle.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    if !idle.is_empty() {
        let wasted: u64 = idle.iter().map(|g| g.size_bytes).sum();
        o.push_str(&format!(
            "<h3>吃灰候选（{} 款，合计 {}）</h3>\n<p class=\"sub\">从未启动或一年以上没玩。这里只做提示，程序不会自动卸载任何东西。</p>\n",
            idle.len(),
            util::fmt_size(wasted)
        ));
        o.push_str("<table class=\"tbl\"><thead><tr><th>游戏</th><th>体积</th><th>时长</th><th>最后游玩</th><th>原因</th></tr></thead><tbody>\n");
        for g in idle.iter().take(60) {
            let reason = if g.never_launched {
                "从未启动"
            } else {
                "沉寂超一年"
            };
            o.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"dim\">{}</td></tr>\n",
                util::escape_html(&g.name),
                util::fmt_size(g.size_bytes),
                util::fmt_duration(g.playtime_minutes),
                util::escape_html(g.last_played_local.as_deref().unwrap_or("—")),
                reason
            ));
        }
        o.push_str("</tbody></table>\n");
    }

    o.push_str("</section>\n");

    // ---------- 现有 Steam 合集 ----------
    if !lib.existing_collections.is_empty() {
        o.push_str("<section><h2>Steam 现有合集</h2>\n<table class=\"tbl\"><thead><tr><th>名称</th><th>类型</th><th>成员数</th></tr></thead><tbody>\n");
        for c in &lib.existing_collections {
            let kind = if c.is_builtin {
                "系统"
            } else if c.is_dynamic {
                "动态（过滤器）"
            } else {
                "静态"
            };
            o.push_str(&format!(
                "<tr><td>{}</td><td class=\"dim\">{}</td><td>{}</td></tr>\n",
                util::escape_html(&c.name),
                kind,
                c.appids.len()
            ));
        }
        o.push_str("</tbody></table>\n</section>\n");
    }

    // ---------- 全部游戏 ----------
    o.push_str("<section><h2>全部游戏</h2>\n");
    o.push_str("<input id=\"q\" class=\"search\" placeholder=\"搜索游戏名或 appid…\" autocomplete=\"off\">\n");
    o.push_str("<table class=\"tbl\" id=\"allgames\"><thead><tr><th>游戏</th><th>appid</th><th>系列/厂商</th><th>时长</th><th>体积</th><th>最后游玩</th><th>状态</th><th>合集</th></tr></thead><tbody>\n");
    for g in &lib.games {
        let mut flags: Vec<&str> = Vec::new();
        if g.never_launched {
            flags.push("未启动");
        } else if g.barely_played {
            flags.push("浅尝");
        }
        if g.dormant {
            flags.push("沉寂");
        }
        if g.favorite {
            flags.push("收藏");
        }
        if g.hidden {
            flags.push("隐藏");
        }
        if g.achievements_total > 0 && g.achievements_unlocked >= g.achievements_total {
            flags.push("全成就");
        }
        let studio = if !g.franchises.is_empty() {
            g.franchises.join(" / ")
        } else if !g.developers.is_empty() {
            g.developers.join(" / ")
        } else {
            "—".to_string()
        };
        o.push_str(&format!(
            "<tr data-k=\"{}\"><td>{}</td><td class=\"mono\">{}</td><td class=\"dim\">{}</td><td>{:.1}h</td><td>{:.2}GB</td><td>{}</td><td class=\"dim\">{}</td><td class=\"dim\">{}</td></tr>\n",
            util::escape_html(&format!("{} {}", g.name, g.appid).to_lowercase()),
            util::escape_html(&g.name),
            g.appid,
            util::escape_html(&studio),
            util::hours(g.playtime_minutes),
            util::gb(g.size_bytes),
            util::escape_html(g.last_played_local.as_deref().unwrap_or("—")),
            if flags.is_empty() { "正常".to_string() } else { flags.join("+") },
            util::escape_html(&g.collections.join(" / "))
        ));
    }
    o.push_str("</tbody></table>\n</section>\n");

    // ---------- 警告 ----------
    if !lib.warnings.is_empty() {
        o.push_str("<section><h2>扫描告警</h2>\n<div class=\"issues\">\n");
        for w in &lib.warnings {
            o.push_str(&format!(
                "<div class=\"alert warn\">{}</div>\n",
                util::escape_html(w)
            ));
        }
        o.push_str("</div></section>\n");
    }

    o.push_str(&format!(
        "<footer>steam-curator · 报告生成于 {}</footer>\n",
        util::escape_html(&util::fmt_local(util::now_epoch()))
    ));
    o.push_str("</main>\n");
    o.push_str(SCRIPT);
    o.push_str("</body>\n</html>\n");
    o
}

fn card(o: &mut String, value: &str, label: &str, class: &str) {
    o.push_str(&format!(
        "<div class=\"card {}\"><div class=\"v\">{}</div><div class=\"l\">{}</div></div>\n",
        class,
        util::escape_html(value),
        util::escape_html(label)
    ));
}

fn bar_row(o: &mut String, label: &str, value: &str, ratio: f64, class: &str) {
    let pct = (ratio * 100.0).clamp(0.5, 100.0);
    o.push_str(&format!(
        "<div class=\"bar\"><div class=\"bl\">{}</div><div class=\"bt\"><div class=\"bf {}\" style=\"width:{:.1}%\"></div></div><div class=\"bv\">{}</div></div>\n",
        util::escape_html(label),
        class,
        pct,
        util::escape_html(value)
    ));
}

const STYLE: &str = r#"<style>
:root{
  --bg:#0e1621; --panel:#16202d; --panel2:#1b2736; --line:#243447;
  --fg:#c7d5e0; --dim:#7f97ad; --accent:#66c0f4; --ok:#8bc34a;
  --warn:#e8b339; --err:#e05a4e; --never:#a86ee0;
}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
  font:14px/1.6 "Segoe UI","Microsoft YaHei",system-ui,sans-serif}
header{padding:28px 32px 18px;border-bottom:1px solid var(--line);
  background:linear-gradient(180deg,#1b2838,#0e1621)}
h1{margin:0 0 6px;font-size:23px;font-weight:600;color:#fff}
h2{font-size:17px;margin:0 0 14px;color:#fff;font-weight:600;
  border-left:3px solid var(--accent);padding-left:10px}
h3{font-size:14px;margin:22px 0 10px;color:#dfe8f0;font-weight:600}
h4{margin:0;font-size:15px;color:#fff}
main{padding:24px 32px 60px;max-width:1500px;margin:0 auto}
section{margin-bottom:38px}
.sub{color:var(--dim);font-size:12.5px;margin:4px 0}
code{background:#0b1119;padding:1px 6px;border-radius:3px;color:#9fd2f5;
  font-family:Consolas,monospace;font-size:12px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:6px;padding:14px 16px}
.card .v{font-size:22px;font-weight:600;color:#fff}
.card .l{font-size:12px;color:var(--dim);margin-top:2px}
.card.warn .v{color:var(--warn)} .card.ok .v{color:var(--ok)}
.pill{display:inline-block;background:#243447;color:var(--accent);
  font-size:11.5px;padding:2px 9px;border-radius:10px;vertical-align:middle;font-weight:400}
.pill.dry{background:#3a3218;color:var(--warn)}
.pill.written{background:#1e3a20;color:var(--ok)}
.tbl{width:100%;border-collapse:collapse;font-size:13px;margin-top:8px}
.tbl th{text-align:left;padding:8px 10px;background:var(--panel2);
  color:var(--dim);font-weight:600;font-size:12px;border-bottom:1px solid var(--line)}
.tbl td{padding:7px 10px;border-bottom:1px solid #1d2937;vertical-align:top}
.tbl tr:hover td{background:#182432}
.mono{font-family:Consolas,monospace;color:var(--dim)}
.dim{color:var(--dim)}
.k{font-size:11.5px;padding:1px 7px;border-radius:3px;white-space:nowrap}
.k-create{background:#1c3a1e;color:#8bc34a}
.k-update{background:#1b3049;color:#66c0f4}
.k-unchanged{background:#243447;color:var(--dim)}
.k-delete{background:#3d1f1c;color:#e05a4e}
.k-skip{background:#3a3218;color:var(--warn)}
.alert{padding:9px 13px;border-radius:5px;margin:6px 0;font-size:13px;
  border-left:3px solid}
.alert.info{background:#16283a;border-color:var(--accent)}
.alert.warn{background:#2e2716;border-color:var(--warn);color:#e8d9a8}
.alert.error{background:#331b18;border-color:var(--err);color:#f0bcb5}
.collections{display:grid;grid-template-columns:repeat(auto-fill,minmax(330px,1fr));gap:14px}
.coll{background:var(--panel);border:1px solid var(--line);border-radius:6px;
  padding:14px 16px;display:flex;flex-direction:column}
.coll header{border:0;background:none;padding:0}
.badges{display:flex;flex-wrap:wrap;gap:6px;margin:8px 0 4px}
.badges span{background:#233247;color:var(--dim);font-size:11px;
  padding:1px 8px;border-radius:9px}
.desc{color:var(--dim);font-size:12.5px;margin:6px 0;font-style:italic}
ul.games{list-style:none;margin:8px 0 0;padding:0;max-height:280px;overflow:auto}
ul.games li{display:flex;justify-content:space-between;gap:10px;
  padding:4px 0;border-bottom:1px solid #1d2937;font-size:13px}
ul.games li:last-child{border-bottom:0}
.gname{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.gmeta{color:var(--dim);font-size:11.5px;white-space:nowrap}
.never{color:var(--never)}
.unknown .gmeta{color:var(--err)}
.chips{display:flex;flex-wrap:wrap;gap:6px}
.chip{background:var(--panel2);border:1px solid var(--line);border-radius:3px;
  padding:2px 9px;font-size:12.5px}
.bars{display:flex;flex-direction:column;gap:5px}
.bar{display:grid;grid-template-columns:200px 1fr 90px;gap:10px;align-items:center;font-size:12.5px}
.bl{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-align:right;color:#b9cade}
.bt{background:#0b1119;height:16px;border-radius:2px;overflow:hidden}
.bf{height:100%;border-radius:2px}
.bf.size{background:linear-gradient(90deg,#2c6fa8,#66c0f4)}
.bf.time{background:linear-gradient(90deg,#3a7a3d,#8bc34a)}
.bf.never{background:linear-gradient(90deg,#5d3a8a,#a86ee0)}
.bv{color:var(--dim);font-family:Consolas,monospace;font-size:11.5px}
.search{width:100%;max-width:420px;background:var(--panel2);border:1px solid var(--line);
  color:var(--fg);padding:8px 12px;border-radius:5px;font-size:13px;margin-bottom:6px}
.search:focus{outline:none;border-color:var(--accent)}
footer{color:var(--dim);font-size:12px;text-align:center;padding:20px}
</style>
"#;

const SCRIPT: &str = r#"<script>
(function(){
  var q=document.getElementById('q');
  if(!q)return;
  var rows=[].slice.call(document.querySelectorAll('#allgames tbody tr'));
  q.addEventListener('input',function(){
    var v=q.value.trim().toLowerCase();
    rows.forEach(function(r){
      r.style.display = (!v || r.getAttribute('data-k').indexOf(v)>=0) ? '' : 'none';
    });
  });
})();
</script>
"#;
