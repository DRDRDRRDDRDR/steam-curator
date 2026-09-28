# 工作流详解

四个阶段，每一步的产物都是磁盘上的一个文件，可以单独重跑、可以人工检查、
也可以随时中断再继续。

> 下文用命令行举例。图形界面版（`steam-curator-gui.exe`）左边的按钮与下面每一个
> 阶段一一对应，调用的是同一批函数、产出同一批文件 —— 区别只在呈现方式。

```
scan ──► library.json ──► prompt ──► AI-PROMPT.md ──► [你贴给 AI]
                                                          │
apply ◄── plan.json ◄── plan ◄── ai-plan.json ◄───────────┘
  │                        │
  └──► report.html ◄───────┘
```

---

## 阶段 1 · `scan` — 读取本地库

```bash
steam-curator scan
```

读的东西（全部只读）：

| 文件 | 取什么 |
|---|---|
| `steamapps/libraryfolders.vdf` | 所有库根（可能有多个盘） |
| `<库>/steamapps/appmanifest_<appid>.acf` | 已安装游戏：名称、体积、语言、StateFlags、最后更新时间 |
| `config/loginusers.vdf` | 账号列表 |
| 注册表 `HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser` | 当前在用账号 |
| `userdata/<id>/config/localconfig.vdf` | 游玩时长、最后游玩时间 |
| `userdata/<id>/config/cloudstorage/cloud-storage-namespace-1.json` | 现有合集归属 |
| `userdata/<id>/config/librarycache/<appid>.json` | 官方系列 / 开发商 / 发行商、成就进度 |

产出 `library.json`（含派生分析位 `never_launched` / `barely_played` / `dormant` /
`days_since_played`）与 `library.csv`。

**注意**：如果某个库根的盘符当前没挂载，会跳过它并给出告警，不会让整次扫描失败。

---

## 阶段 2 · `prompt` — 生成给 AI 的指令

```bash
steam-curator prompt
```

生成 `AI-PROMPT.md`，内容分五节：

1. **库概览** —— 总数、体积、总时长、未启动/沉寂/未归档计数
2. **游戏清单** —— Markdown 表格：appid、名称、系列/厂商、时长、体积、最后游玩、状态、现有合集
3. **可直接利用的信号** —— 占盘 Top8、从未启动清单、最近 60 天玩过、官方系列分组、**尚未归档清单**
4. **输出要求** —— 严格的 JSON 契约 + 硬性规则
5. **紧凑 JSON** —— 同一份数据的机器友好形式，便于模型精确核对 appid

### fresh 与 extend 两种模式

工具会自动判断：**已有 20 个以上静态合集 ⇒ extend 模式**。

| | fresh | extend |
|---|---|---|
| 适用 | 库还没怎么整理过 | 已经有一套成体系的合集 |
| 任务 | 从零设计一套分类 | **只补全未归档的**，不推倒重来 |
| 目标合集数 | 8~16 | 2~5 |
| 现有合集 | 仅作参考 | 明确要求可复用同名合集（工具会合并而非重复建） |

想强制切换：`--mode fresh` / `--mode extend`。

---

## 阶段 3 · 把 prompt 交给 AI

把 `AI-PROMPT.md` **整份**贴给任意对话式 AI（不需要联网能力，不需要 API key）。

AI 应该回一个 JSON。实际使用中它经常会：

- 在 JSON 外面包一层 ``` 围栏 → 工具会自动剥掉
- 前后加一段寒暄 → 工具会取第一个 `{` 到最后一个 `}`
- 把 `appids` 写成 `apps` / `added` / `games` → 都认
- 编几个不存在的 appid → 工具会拦下并报警
- 同一款游戏在一个合集里写两次 → 去重并报警
- 同一个合集名给两遍 → 合并

把 AI 的回复**原样**（含围栏和寒暄）保存为 `output/ai-plan.json`：

```bash
# 也可以直接走标准输入
steam-curator plan -i -
# 或者指定文件
steam-curator plan -i D:\somewhere\reply.txt
```

---

## 阶段 4 · `plan` — 校验与清洗

```bash
steam-curator plan
```

做的事：

1. **剥壳**：去掉围栏与寒暄，取出 JSON。
2. **幻觉拦截**：拿真实 appid 全集比对，库里不存在的 appid 一律丢弃并逐个报告
   （`--keep-unknown` 可保留，但基本不会想这么干）。
3. **去重**：同一合集内的重复 appid；同名合集合并。
4. **空合集**：清洗后为空的合集丢弃（`--allow-empty` 可留）。
5. **系统合集名冲突**：`favorite` / `hidden` 被占用时报错并丢弃。
6. **模糊匹配现有合集**：只差空白／大小写（例如模型给出 `A3 Tool`、磁盘上是 ` A3 Tool`）
   视为同一个，并**沿用磁盘上的原名**。这一步很关键 —— 否则每跑一次都会新建一个重复合集。
7. **区分「未归档」**：一款游戏只要已在你现有的任一静态合集里，就不算未归档。
   真正的缺口 = 既不在本方案、也不在现有任何合集中。
8. **统计**：本方案覆盖多少、多少已由现有合集收录、多少是真缺口、新建/更新各几个。

产出 `plan.json` 与 `report.html`。

---

## 阶段 5 · `preview` — 先看效果

```bash
steam-curator preview --open
```

`report.html` 是**完全自包含**的（无 CDN、无外链、无字体请求），双击即看、断网也能看。

---

## 阶段 6 · `apply` — 写回

```bash
steam-curator apply            # 演练：只打印会怎么做
steam-curator apply --write    # 真正落盘
```

写入前会：

1. 检查 **Steam 是否在运行** —— 在运行就拒绝（`--force` 可跳过，但 Steam 退出时
   会用内存状态覆盖你的改动，等于白做）。
2. 计算命名空间版本号 `max(当前命名空间版本, 文件内最大条目版本) + 1`，
   并同步抬高 `cloud-storage-namespaces.json`，让本地改动在合并时被认定为更新。
3. 整份备份合集文件、命名空间版本文件、`localconfig.vdf`，并记下
   「备份文件名 → 原始绝对路径」的映射。

### merge 与 replace

同名合集默认走 **merge**：保留已有成员，把新成员并进去。
报告里会写明「保留原有 N 款」。想完全以方案为准用 `--replace`，
被移除的游戏会在报告里列出来。

### 幂等

如果某次计算的结果与 Steam 现状完全一致（成员集合、removed 都相同），
该条目**不会**被改写，版本号也不会被抬。所以反复 `apply` 是安全的。

### 只动自己的东西

- `showcases.*`、`NewContentRollup_*`、`GameReleased`、其它键族 → 原样保留
- 删除标记（`is_deleted`）→ 原样保留
- 动态合集（带 `filterSpec`）→ 绝不改写，同名时报告「跳过」并说明
- `favorite` / `hidden` 系统合集 → 不碰
- `cloud-storage-namespace-1.modified.json` → 不碰

---

## 阶段 7 · `restore` — 后悔药

```bash
steam-curator restore --latest          # 演练：列出会还原哪些文件
steam-curator restore --latest --confirm  # 真正执行
steam-curator restore --backup 2026_09_28_1612_00 --confirm  # 指定某一份
```

还原依据备份目录里的映射文件，不做任何路径猜测。
