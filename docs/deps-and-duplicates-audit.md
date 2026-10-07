# 依赖重复审计与跟进基线 (Dependency Duplicates Audit)

> **目的**: 记录本次依赖重复调查结论,作为后续依赖更新的"再审查触发基线">
> 当 `Cargo.lock` 中的相关版本号变动时(见文末「何时需要再审」),按本文档的脚本重新验证。
>
> **一句话结论**: 当前图里 `cargo tree --duplicates` 报的重复,绝大多数是**上游第三方各自的锁定 + 平台专属**(非本仓库代码并列实现),**没有值得在本仓库内修复的项**。真正的字体栈重复根源在 `cosmic-text 0.19` 自身内部,上游无解。

---

## 0. 审计环境与方法

- **时间**: 本次会话
- **主机**: `aarch64-apple-darwin` (mp-dev)
- **提交**: 依赖声明未改动;仅生成/更新本文档。`Cargo.lock` 为当前现状。
- **方法**:
  1. 本机原生目标重复清单:`cargo tree --workspace --duplicates`
  2. **Linux 目标重复验证(无需真装 Linux)** — 在 macOS 上用 cargo 强制解析 Linux 目标:
     ```bash
     cargo metadata --format-version 1 --filter-platform aarch64-unknown-linux-gnu \
       > /tmp/meta_linux.json
     ```
     对 `resolve.nodes[].id` 按 `name@version` 统计每包版本集合,`len>1` 即重复。
  3. 用反向依赖图(`resolve.nodes[].deps`)定位"谁拉出某个版本"。

---

## 1. 基线数据(可复核)

| 指标 | 数值 |
|---|---|
| `cargo tree --workspace --duplicates` top-level 条目 | **43** |
| 重复**包名**数(全 target 解析) | macOS **65** 项 / Linux **34** 项 |
| 判定为本仓库"可修、且值钱"的重复 | **0** |

> 注意差异: **全 target 解析**会把所有平台的 deps 都算进来(即使不编译),所以 macOS 算到 65;但**实际构建**时大量 `windows-targets`/`objc2-family`/`redox*` 根本不进产物。真正"进产物"的重复只有 Linux 字体栈一处(见 §3)。

---

## 2. 分类结论表(已核验,非推测)

> 每行都追溯了"谁拉出该版本",判定 来源 与 可行性。

| # | 重复依赖 | 版本 | 来源(谁拉的) | 类型 | 可行性 |
|---|---|---|---|---|---|
| 1 | **fontdb** | `0.23` / `0.24` | `cosmic-text` / `usvg`+`resvg` | 上游撞 | **不可修**(见 §3) |
| 2 | **skrifa** | `0.40` / `0.44` | `cosmic-text` / `swash`+`usvg` | 上游撞 | **不可修** |
| 3 | **read-fonts** | `0.37` / `0.41` | `skrifa` 0.40 / skrifa 0.44 | 上游撞 | **不可修** |
| 4 | **font-types** | `0.11` / `0.12` | `read-fonts` 0.37 / 0.41 | 上游撞 | **不可修** |
| 5 | **harfrust** (harfbuzz 绑定) | `0.5.2` / `0.12` | `cosmic-text` / `usvg` | 上游撞 | **不可修** |
| 6 | **dirs** | `5` / `7` | `zed-font-kit` / `fc-gpui-util` | 消费者主版本不同 | 修复需 vendor,见 §4 |
| 7 | **objc** | `0.5` / `0.6` | `accesskit_macos` 家族 / `fc-gpui-macos`+wgpu | 上游锁定 | 不可修(等 accesskit) |
| 8 | **windows-sys / windows-\*** | `0.48`/`0.52`/`0.61` + windows 0.61/0.62 家族 | 第三方传递(tokio/zbus/tempfile/…/ 本仓 `windows 0.62`) | 生态固有 | 不可修(常态) |
| 9 | **syn / thiserror / getrandom / itertools / base64 / core-foundation** 等 | 各 2~3 版 | proc-macro 生态 + 各大链各自锁定 | 生态固有 | **不可修 / dev-only** |

**关键判定**: `第 1–5` 全部源于**上游 crate 之间版本不兼容**,不是本仓库并列依赖;`第 6–9` 是生态常态。**没有一处是本仓库"可以一行改掉、且对用户有实际收益"的。**

---

## 3. 字体栈重复的根因(本轮最重要的发现)

在 **Linux**(freebsd)目标下,`cosmic-text 0.19` 被编译,**它自己**就把字体解析栈撕裂成两身:

```text
cosmic-text 0.19 ---(直接) skrifa 0.40 -> read-fonts 0.37 -> font-types 0.11    <- 旧
cosmic-text 0.19 ---(另用) swash 0.2.10 -> skrifa 0.44  -> read-fonts 0.41 -> font-types 0.12  <- 新(重复!)
```

以及同源双份:
- `fontdb`  : `0.23`(<-cosmic-text)  vs `0.24`(<-usvg)
- `harfrust`  : `0.5.2`(<-cosmic-text) vs `0.12`(<-usvg)

即: **`cosmic-text 0.19` 同时依赖 skrifa 0.40 和 swash(->skrifa 0.44),导致字体解析三层被拉成两份。** 因为:
- `skrifa 0.40` 配老 `read-fonts 0.37 + font-types 0.11`
- `skrifa 0.44` 配新 `read-fonts 0.41 + font-types 0.12`
- 两套不可混排,于是整套重复。

### 为什么不可修
- **`cosmic-text 0.19.0` 已是 crates.io 最新**;`swash 0.2.10`、`skrifa 0.48` 也是最新。→ 没有"升级 cosmic-text 以对齐 skrifa"的版本。
- 本仓在 Linux 上是**被动消费者**,既不改、也不 upgrade 任何一方(cosmic-text / usvg / swash)。上游必须等 cosmic-text 发行商把内部 `skrifa` 依赖从 `0.40` 升到 `.44+`。

### 平台可达性(用户实际见的)
- **macOS / Windows**: `cosmic-text` 不入产物(linux-gated),`skrifa 0.40 / read-fonts 0.37 / fontdb 0.23 / harfrust 0.5.2` 一套**不编译**,产物只有新版。
- **Linux (freebsd)**: 两套都编译 → 二进制里多几份字体解析代码。

---

## 4. vendored 三件套实测结论:正确,不该动

`Cargo.toml` 里 `[patch.crates-io]` 有三个本地 crate。逐一与 registry 版本对读 `Delta vs upstream` 注释 + 实际依赖图,均**行为保持、且是"减重复"而非制造重复**:

| vendored crate | 做了什么 | 核实结果 |
|---|---|---|
| `vendor/core-graphics2` | `block` optional,仅被不用的 `display-stream` 触发 | ✅ 全仓一次;减少 `block` 进图 |
| `vendor/core-video` | `block` optional;`gpui-macos` 用 `default-features=false,features=["link"]` 禁用 `display-link` | ✅ 像素/命名空间路径完好,帧同步走原生;`block` 确实不编译 |
| `vendor/zed-scap` | 去掉内核 mac-only `cocoa/objc/screencapture`;`windows=0.62`、`rand=0.9` 对齐工作区;`windows-capture <1.5` | ✅ 避免第二份 `windows 0.61`/`rand 0.8` 进 lock |

> 顺带核验:`objc2 0.5` 那份重复仅由 `accesskit_macos`(accm)及其家族拉入,与上述 vendoring 无关。**本仓改不了,只能等上游。**

---

## 5. 当前的决定与"何时需要再次审查"

### 当前决策
- **本仓库不主动改任何依赖版本来"去重"**。收益(少编一个 crate)都低于回归风险(尤其 `find_best_match` 字体匹配路径,改 dirs 还要 vendor 整包)。
- `deny.toml` 保持 `multiple-versions = "warn"`(**不要** 升 `deny`)——现在直接 deny 必误报约几十项,阻塞全部 CI 绿灯。

### ✅ 依赖更新后需**自动触发再审查**的检查点
> 当下列任一 `Cargo.lock` 值变化,重新跑 §0 脚本并把结论更新回本文档(版本栏随之变更)。

1. **`cosmic-text` 升到 `> 0.19`** —— 这是唯一的"真正修复"入口,期望看到 `skrifa` 收敛成一份。
   - 复查项:`skrifa 0.40/0.44` 是否合并;`read-fonts`、`font-types`、`fontdb`、`harfrust` 同理。
2. **`skrifa` 或 `swash` 任一出现第三版**(如 `0.48`)—— 重新画 §3 图。
3. **`accesskit_macos` 升到用 `objc2 0.6`** 的版本 → 预期消掉 `objc2 0.5` 子树,清单第 7 行可划勾。
4. **`zed-font-kit` 升级**(registry) → 看是否还锁 `dirs 5`;若升到 `dirs 7+`,清单第 6 行可划勾。
5. **`windows` 引用到新大版本或 `wgpu` 大版本变动** → 重新核对 windows-family 清单。
6. **新增第三方依赖**时 → 对新增包跑一次 §0 的 Linux-target 解析,确认不引入新重复。

> 若 1–5 中任一项被触发且确认仍为上游锁定,无需动代码,但在本文档追加一行"已复查于 <日期>,无变化 / 已到 vX.Y" 以便持续追踪。

---

## 参考(复核用原始命令)

```bash
# 本机(当前 target)重复
cargo tree --workspace --duplicates

# Linux 目标重复(可解析,无需交叉编译)
cargo metadata --format-version 1 --filter-platform aarch64-unknown-linux-gnu > /tmp/meta_linux.json

# 反查某个版本被谁拉
cargo tree -i <name>@<version>

# 确认某依赖是否真的进产物(比如 cosmic-text 是否编译)
cargo tree -i skrifa@0.40.0   # 空 = 不编译(target-gated)
```

**基线登记版本**(审计时刻 `Cargo.lock`):
`cosmic-text 0.19.0` | `skrifa 0.40/0.44` | `swash 0.2.10` | `read-fonts 0.37/0.41` | `font-types 0.11/0.12` | `fontdb 0.23/0.24` | `harfrust 0.5.2/0.12` | `ttf-parser 0.25.1`

重大修订: 基线登记(本次审计)。