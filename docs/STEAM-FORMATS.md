# Steam 本地文件格式 — 本机实测记录

全部结论都在 2026-09-28 于本机（Windows，Steam 最新客户端）**实际读过文件确认**，
不是从文档抄的。写代码时踩过或差点踩的坑单独标出。

---

## 1. `steamapps/libraryfolders.vdf`

文本 VDF，根键 `"libraryfolders"`，子键是 `"0"`、`"1"`… 的字符串索引。

```vdf
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"contentid"		"5840835621309866379"
		"totalsize"		"0"
		"apps"
		{
			"1510"		"0"
			"1245620"		"55123337185"
		}
	}
}
```

坑位：

- `totalsize` 对系统盘那个库可能是 `"0"`，**别拿它当容量**。
- `apps` 里 appid 对应的值是字节数，也可能是 `"0"`（待安装/待更新）。
  「是否安装」要看 appmanifest 的 `StateFlags`，不是这个。
- 本机实测库根可以是 `C:\Program Files (x86)\Steam`、`G:\SteamLibrary` 等，
  多个库根可能**指向同一台机器上不存在的盘符** —— 必须容错，别让一个断开的盘毁掉整次扫描。

## 2. `steamapps/appmanifest_<appid>.acf`

文本 VDF，根键 `"AppState"`。字段**大小写不统一**，这是最容易踩的坑：

| 字段 | 磁盘上的实际大小写 | 含义 |
|---|---|---|
| `appid` | 全小写 | appid |
| `name` | 全小写 | 安装时的显示名（可能落后于商店改名） |
| `installdir` | 全小写 | `steamapps\common\` 下的目录名 |
| `StateFlags` | 帕斯卡 | 位域，`4` = 已完整安装 |
| `LastUpdated` | **实测为 `lastupdated` 或 `LastUpdated` 都有** | 内容更新时间（epoch 秒） |
| `LastPlayed` | 帕斯卡 | epoch 秒，`"0"` 表示从未启动 |
| `SizeOnDisk` | 帕斯卡 | 字节 |
| `LastOwner` | 帕斯卡 | 安装者 steamid64（**PII**，本工具只用来推 steam3） |
| `UserConfig.language` / `MountedConfig.language` | — | 语言，如 `schinese` |

**规则：VDF 的键一律按大小写不敏感解析。** Valve 自己的 KeyValues 就是大小写不敏感的。
`vdf.rs` 里所有取值都走 `*_ci` 系列。

## 3. `userdata/<steam3>/config/localconfig.vdf`

文本 VDF，根键 `"UserLocalConfigStore"`。游玩时长的**确切路径**是：

```
UserLocalConfigStore / Software / Valve / Steam / apps / <appid>
```

| 键 | 单位 | 说明 |
|---|---|---|
| `Playtime` | **分钟** | 总时长 |
| `Playtime2wks` | **分钟** | 近两周（只出现在近期玩过的条目上，键名就是这个怪拼写） |
| `LastPlayed` | epoch 秒 | |

坑位：

1. **`LastPlayed` 哨兵值 `86400`**（= 1970-01-02）。这是「早于 Steam 记录时间戳时代」
   的标记，不是真实日期。任何小于 1980 的值都当「未知」处理。
2. **条目内的键顺序不稳定**，同一台机器上不同账号的同一 appid 都能不一样。绝不能按位置解析。
3. **有些条目根本没有游玩键**（整块只有 `cloud { last_sync_state }`，
   appid 7、760 这类工具条目）。缺键就跳过，不要输出 0。
4. **`apptickets`、`depots`、`UserAppConfig` 同样是「以 appid 为键的 map」**，
   形状一模一样。必须走完整路径 `Software/Valve/Steam/apps`，不能取「第一个像 appid 的节点」。
5. 本机实测这份文件里 `WebStorage` 下还有一个 `"user-collections"  "{}"` 键 ——
   这是**空的遗留镜像**，新版合集的真源不在这里（见下一节）。别被它骗了。

## 4. `userdata/<steam3>/config/cloudstorage/` — 新版合集的真源

目录内容（本机实测）：

```
cloud-storage-namespace-1.json            191,692 B   ← 合集都在这里
cloud-storage-namespace-1.modified.json         2 B   `[]`
cloud-storage-namespace-3.json                  2 B
cloud-storage-namespace-3.modified.json         2 B
cloud-storage-namespaces.json                  20 B   `[[3,"0"],[1,"7481"]]`
```

### 4.1 `cloud-storage-namespace-1.json`

**单行 JSON，顶层是 `[key, entry]` 二元组的数组**，不是对象 map：

```json
[["user-collections.hidden", {
  "key": "user-collections.hidden",
  "timestamp": 1753104285,
  "value": "{\"id\":\"hidden\",\"name\":\"已隐藏\",\"added\":[],\"removed\":[]}",
  "version": "5885"
}], ...]
```

要点：

- `value` 是**字符串化的 JSON，需要二次解析**。
- `version` 是**字符串**，且是**每个条目各自单调**的（实测同一文件里同时存在
  `7478`、`5885`、`1682`），不是全局序列。命名空间级版本在
  `cloud-storage-namespaces.json` 里（本机 `"7481"`）。
- **删除标记（tombstone）**：`{"key":..., "timestamp":..., "is_deleted":true, "version":...}`，
  **完全没有 `value` 字段**。解析时必须以 `is_deleted` 为准，不能靠 `value` 缺失判断。
- 合集键一律 `user-collections.` 前缀。id 段实测出现过三类：
  - `favorite` / `hidden` —— 系统合集，**不要动**；
  - `uc-<12 位>` —— 用户建的（官方 id 的字符集含 `+` `/` `*`，所以别假设它能当文件名）；
  - `srm-<base64>` —— 第三方工具（Steam ROM Manager）建的；
  - `partner-*` —— 合作方合集，`value` 里**只有 `{id,name}`，没有 `added`/`removed`**。
- 同一个数组里还混着 `showcases.*`、`NewContentRollup_<appid>`、`GameReleased`、
  `whatsapp`、`collection-bootstrap-complete` 等键族。**重写整个文件时必须原样带走它们。**

合集 `value` 解码后的形状：

```json
{
  "id": "uc-Mnd2PWUey2Y3",
  "name": "backlog",
  "added":   [377160, 22370],
  "removed": [],
  "filterSpec": { "nFormatVersion": 2, "strSearchText": "", "filterGroups": [...], "setSuggestions": {} }
}
```

- **静态合集**：没有 `filterSpec`，成员 = `added`。
- **动态合集**：有 `filterSpec`，成员由 Steam 按过滤器实时算，`added` 通常是空的。
  本工具**只读它、绝不改写**。
- `added` 是**整数**数组（不是字符串）。

### 4.2 `cloud-storage-namespaces.json`

`[[3,"0"],[1,"7481"]]` —— `[命名空间 id, 版本号字符串]` 的数组。
命名空间 1 就是合集/库这个。

### 4.3 写回约定（本工具采用的做法）

1. 新条目版本号 = `max(命名空间版本, 文件内最大条目版本) + 1`，随后**同步抬高**
   `cloud-storage-namespaces.json` 里命名空间 1 的版本号。这样合并时本机的改动会被认定为更新。
2. 只删除「key 属于本次要覆盖/删除的集合」的条目，其余原样保留。
3. 追加新条目后**按 key 排序**（Steam 自己的文件就是有序的）。
4. 序列化为**紧凑单行**（与 Steam 自身写法一致），原子写入（先写 `.tmp` 再 rename）。
5. `value` 的键顺序手工锁定为 `id,name,added,removed`，与 Steam 一致。
6. **`cloud-storage-namespace-1.modified.json` 不动**（实测为 `[]`）。

> ⚠️ 一条尚未在本机闭环验证的假设：写入后 Steam 启动时是否**必然**采纳本地这份
> （而不是把云端版本盖回来）。版本号单调递增是为此设计的，但需要一次真机验证：
> 退出 Steam → `apply --write` → 启动 Steam → 看「库」界面。
> 若未生效，`restore --latest` 可完整回滚。

## 5. 其它

- `config/loginusers.vdf`：根键 `"users"`，子键是 steamid64，
  含 `PersonaName`、`MostRecent`（`"1"` 表示最近登录）。
- steam3 id = `steamid64 − 76561197960265728`（本机：`76561198798451485` → `838185757`）。
- `userdata/` 下可能有多个账号目录，**每个都要能独立处理**，默认取 `MostRecent`。

## 6. 崩溃过的写法（别重蹈）

- 按位置解析 app 块内的键 → 直接错位，因为键顺序不稳定。
- 用「第一个以数字为键的节点」当 apps → 命中 `apptickets`，拿到一堆 hex。
- 把 `LastPlayed == 86400` 当真实日期 → 输出 1970 年。
- 把合集的 `value` 当对象直接用 → `serde_json` 报类型错误，它是字符串。
- 靠「`value` 字段缺失」判断删除 → 应看 `is_deleted`。
- 重写合集文件时只写自己的合集 → 抹掉 showcases 与其它键族。
