# steam-curator

> 把 Steam 库读成数据 → 交给 AI 判断怎么分类 → 校验结果 → 预览 → 写回 Steam 合集。

一个 Rust 写的工具，**同时提供命令行版和原生图形界面版**（Windows 上是真正的
Win32 窗口，不是套壳浏览器）。它不联网、不调 API、不上传任何东西：
**AI 那一步由你自己完成**（把生成的 prompt 贴给任意对话式 AI，再把回复贴回来）。

```
        ┌──────────┐   library.json   ┌──────────┐   AI-PROMPT.md   ┌─────────┐
本地Steam │  scan    │ ───────────────► │  prompt  │ ───────────────► │  你/AI  │
        └──────────┘                  └──────────┘                  └────┬────┘
                                                                        │ ai-plan.json
        ┌──────────┐   plan.json      ┌──────────┐   report.html   ┌────▼────┐
Steam合集 │  apply   │ ◄─────────────── │  plan    │ ◄────────────── │ 校验清洗 │
        └──────────┘                  └──────────┘                  └─────────┘
```

## 为什么不是「AI 直接改我的库」

三个理由，也是这个工具的骨架：

1. **AI 会在 appid 上产生幻觉。** 它没见过你的库，编几个数字是常态。
   `plan` 会用真实的 appid 全集去对，凡是库里不存在的 appid 一律拦下并报告，
   而不是默默写进 Steam 让一堆合集变成空的。
2. **写 Steam 配置是有代价的操作。** 所以默认 `apply` 只**演练**，
   真要落盘得显式加 `--write`；而且每次写入前自动整份备份、一条命令回滚。
3. **AI 输出格式一定会飘。** 包 ``` 围栏、键名写成 `apps`、同名合集给两个、
   同一款游戏在一个合集里出现两次 —— 这些都在 `plan` 里被规整掉，
   每一种修复都会打印给你看。

## 构建

需要 Rust（任意 1.74+ 版本）和一个链接器。仓库不含任何外部资源。

```bash
cargo build --release
# 产物:
#   target/release/steam-curator(.exe)      命令行版，约 900 KB
#   target/release/steam-curator-gui(.exe)  图形界面版，约 925 KB
```

命令行版只依赖 `serde` + `serde_json`；图形界面版额外依赖 `native-windows-gui`
（Win32 控件的薄封装，只拉 `winapi` / `lazy_static` / `bitflags`）。
两者都没有 C 依赖、没有网络调用，编出来的 exe 只依赖 Windows 自带的系统 DLL，
拷到别的机器上直接能跑。

只想要命令行版、不想拉 GUI 依赖：

```bash
cargo build --release --no-default-features
```

## 图形界面

双击 `steam-curator-gui.exe` 即可。左边一列按钮就是流水线的每一步，
右边上面是运行日志、下面是粘贴 AI 回复的输入框。

```
┌───────────────────────────────────────────────────────────────┐
│ 输出目录：…\output │ 已生成方案 │ Steam 未运行（可安全写入）    │
├────────────────────┬──────────────────────────────────────────┤
│ ① 扫描 Steam 库     │ 运行日志 / 结果                          │
│ ② 生成 AI Prompt    │ ┌──────────────────────────────────────┐ │
│ 　 复制 Prompt…      │ │ （每一步的结果都打在这里）             │ │
│ ③ 校验 AI 回复      │ └──────────────────────────────────────┘ │
│ ④ 打开预览报告      │ 把 AI 的回复整段粘到这里                  │
│ 　 打开输出目录      │ ┌──────────────────────────────────────┐ │
│ ⑤ 演练写回          │ │                                      │ │
│ ⑥ 正式写回          │ └──────────────────────────────────────┘ │
│ ⑦ 回滚最近备份      │ 安全说明…                                 │
│ ⑧ 查看库现状        │                                           │
│ 　 清空日志         │                                           │
└────────────────────┴──────────────────────────────────────────┘
```

几个刻意的设计：

- **输出目录自动对齐命令行版**。GUI 从 exe 位置向上找 `Cargo.toml`，用
  `<项目根>/output`，所以双击 `target\release\steam-curator-gui.exe` 与在项目根
  执行 `steam-curator` 看到的是同一份数据（也可用 `--out <目录>` 覆盖）。
- **「正式写回」是两步确认**：第一次点只是把按钮变成「⚠ 再点一次确认写回」，
  第二次点才真的写。防手滑。
- **窗口固定尺寸**，不设 `WS_THICKFRAME` —— 布局是固定像素的，允许拉伸只会错位。
- GUI 与 CLI 调用的是**同一批函数**（`steam_curator::session`），不存在
  「命令行里对、图形界面里不一样」的可能。

### Windows 上用 GNU 工具链

如果本机没装 MSVC：

```powershell
# 1) Rust（GNU host，minimal profile）
Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe' -OutFile "$env:TEMP\rustup-init.exe"
& "$env:TEMP\rustup-init.exe" -y --default-host x86_64-pc-windows-gnu --profile minimal

# 2) MinGW 链接器
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
```

## 快速开始

图形界面的：双击 `steam-curator-gui.exe`，按左边按钮 ①→⑥ 一路点下去。

命令行的：

```bash
steam-curator all                 # 扫描 + 生成 prompt（写到 ./output/）
# → 打开 output/AI-PROMPT.md，整份贴给 AI，把回复存成 output/ai-plan.json
steam-curator plan                # 校验 AI 结果，生成 plan.json + report.html
steam-curator preview --open      # 打开预览报告，先看效果再决定要不要写
steam-curator apply               # 演练：只打印将会怎么做
steam-curator apply --write       # 落盘（先完全退出 Steam！）
steam-curator restore --latest    # 后悔了：回滚
```

## 命令

| 命令 | 作用 | 主要产物 |
|---|---|---|
| `scan` | 扫描本地 Steam 库 | `library.json`、`library.csv` |
| `prompt` | 生成给 AI 的 prompt | `AI-PROMPT.md` |
| `plan` | 导入 AI 回复并校验清洗 | `plan.json`、`report.html` |
| `preview` | 生成自包含 HTML 预览 | `report.html` |
| `apply` | 写回 Steam 合集（默认演练） | 改 Steam 文件 + `apply-report.json` |
| `restore` | 从备份回滚 | 还原 Steam 文件 |
| `inspect` | 只读查看库与合集概况 | 无 |
| `all` | `scan` + `prompt` | 同 scan/prompt |

通用选项：`-o/--out <目录>`、`-s/--steam-dir <目录>`、`-u/--user <ID>`。
完整选项见 `steam-curator help`。

## 数据从哪来

全部是 Steam 官方写入的本地文件，只读不改（除了 `apply`）：

| 数据 | 来源 |
|---|---|
| 库根列表 | `steamapps/libraryfolders.vdf` |
| 已安装游戏、体积、语言、最后更新 | `<库>/steamapps/appmanifest_<appid>.acf` |
| 游玩时长、最后游玩时间 | `userdata/<steam3>/config/localconfig.vdf` |
| 现有合集（权威） | `userdata/<steam3>/config/cloudstorage/cloud-storage-namespace-1.json` |
| 账号 | `config/loginusers.vdf` |

细节与实测坑位记录在 [docs/STEAM-FORMATS.md](docs/STEAM-FORMATS.md)。

## 安全边界

写入前会逐条检查，任何一条不满足都拒绝动手：

- **Steam 正在运行时拒绝写入**（可用 `--force` 跳过，但你会丢改动：Steam 退出时会用
  内存里的状态覆盖这个文件）。
- **默认演练**，不加 `--write` 绝不碰任何文件。
- **写前整份备份**到 `output/backups/<时间戳>/`，含合集文件、命名空间版本文件、
  `localconfig.vdf`，以及一份记录「备份文件 → 原始绝对路径」的映射，
  所以 `restore` 不依赖任何猜测。
- **只动自己的东西**：`showcases.*`、`NewContentRollup_*`、删除标记（tombstone）、
  别的命名空间条目全部原样保留，只重排为紧凑格式。
- **不碰动态合集**（带 `filterSpec`、由过滤器实时生成的）和系统合集
  （`favorite`、`hidden`）。
- **幂等**：结果与现状一致时不改版本号，重复执行不会反复抬版本。
- **不会卸载任何游戏。** 报告里的「吃灰候选」只是提示。

## 归档策略：merge 还是 replace

默认 `merge` —— 已有同名合集时，**保留原有成员**，把新成员并进去。
这是保守选择：AI 偶尔漏掉几款游戏时，你不会因此丢掉之前手工整理的结果。
报告里会明确写出「保留原有 N 款」。

想以方案为准则用 `--replace`，被移除的游戏会在报告中列出。

`--prune` 用于清理：删除**本工具此前创建**、但当前方案里已经不存在的合集
（靠 `output/state.json` 精确识别，不会误删你自己建的合集）。

## 会生成什么

```
output/
├── library.json          扫描结果（结构化，含派生分析位）
├── library.csv           同上，表格版，方便丢进 Excel
├── AI-PROMPT.md          给 AI 的 prompt（含数据表 + 输出契约）
├── ai-plan.json          ← 你放 AI 回复的地方
├── plan.json             校验清洗后的整理方案
├── apply-report.json     最近一次写入演练/执行的结果
├── report.html           自包含预览报告（断网可看）
├── state.json            本工具管理过的合集记录（供 --prune 精确清理）
└── backups/<时间戳>/     每次写入前的完整备份
```

`output/` 已在 `.gitignore` 里 —— 里面有你的个人库数据，别提交。

## 预览报告里有什么

- 库概览统计卡（体积、时长、从未启动、沉寂、未归档）
- 写回预览：每个合集是新建/更新/无变化/删除，以及新增、保留、移除各多少
- 整理方案：合集卡片，含成员、时长、体积，未启动的游戏单独标色
- 校验结果：AI 幻觉 appid、重复项、未归档游戏等，按 info/warn/error 分级
- 数据洞察：占盘大户 Top15、时长 Top15、吃灰候选清单
- Steam 现有合集对照
- 全部游戏表（带即时搜索框）
- 扫描告警

## 常见问题

**找不到 Steam / 扫不到游戏？**
用 `-s "D:\Steam"` 显式指定；`steam-curator inspect` 会打印它实际用了哪个路径。

**提示「找不到合集文件」？**
`cloud-storage-namespace-1.json` 由 Steam 客户端在首次进入「库」界面时创建。
先启动一次 Steam 再看。

**写了但 Steam 里没变化？**
确认写入时 Steam 是**完全退出**的（含托盘图标），然后再启动 Steam。
若仍未生效，`steam-curator restore --latest` 回滚，并到 issue 里附上
`apply-report.json`。

**多个账号？**
`steam-curator inspect` 列不出来时用 `-u <steam3>` 指定，默认取最近登录的账号。

**能用在 Linux / Steam Deck 吗？**
代码里除「定位 Steam 根目录」（Windows 走注册表）和「时区偏移」（Windows 读注册表）
之外都是跨平台的。Linux 上可以用 `-s ~/.steam/steam` 绕开第一条；
时区会退回 UTC。尚未在 Linux 上实测。

## 许可

MIT，见 [LICENSE](LICENSE)。
