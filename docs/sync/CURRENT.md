# 当前 Zed GPUI 同步状态

> 这是当前唯一可执行的同步说明。其他 `docs/sync` 文档是带日期的历史报告或拆分前指南，只用于追溯当时的判断。

## 当前基线

- 当前开发分支：`develop/0.9`
- 已发布：GPUI 八个发布包 `0.9.0`；`fc-gpui-util` 和 `fc-gpui-util-macros` `0.6.0`
- 下一版本：`0.9.1`（尚未 bump、尚未 publish；更新本文不构成发布）
- 兼容基线：GPUI `0.8.1`、utility crates `0.5.1`；breaking API/feature 变化必须在 `CHANGELOG.md` 提供迁移映射
- 永久兼容入口：`Application::new/headless`；不在任何版本安排弃用或删除
- Registry 发布顺序：`fc-gpui-util-macros 0.6.0` → `fc-gpui-util 0.6.0` → `fc-gpui-macros 0.9.0` → core/renderer/backends → platform → public facade
- 上游仓库默认位置：`../zed`
- 公共包：`fc-gpui`，lib 名仍为 `gpui`
- 核心实现包：`fc-gpui-core`，lib 名为 `gpui_core`

吸收方式是按主题 cherry-pick，**不是**把某一个 Zed commit 当作已完全吸收的祖先。机器可读字段见 `upstream-audit.json`。

| 字段 | SHA | 说明 |
|---|---|---|
| 当前仓库（含 absorb #15–#19） | `4dd9685ba14d706f135ad45c55e0163b22f8403c` | 2026-09-07；本地 HEAD 在本次文档更新之前 |
| 上一完整分类区间起点（不含） | `ec3d887507f272119d9fe146c685f0a941d0e798` | 2026-07-22；JSON `baseline` |
| 上一完整分类区间终点 | `4bd1993783703e92affb781503916d1f152f599f` | 2026-08-10；JSON `audited_upstream`；区间内 49 条已分类 |
| 按 Zed 提交日期最新已吸收 | `3ce72bab201ad82418fc9716aad11332624951bc` | 2026-09-03；#17 `Hitbox::is_hovered_at` |
| 按本仓库 `Zed-Origin` 最新吸收 | `ce48461eaadd16c65c31f835511ab96bd3b6e746` | #19 util shell panic |
| 对照的 Zed `main` | `5a9b9558db01a6b906cec2fb70a797affdc58cdd` | 2026-09-04；`Use proper editions (#63733)`；JSON `compared_against` |

下一轮增量扫描从 `4bd19937` 开始，**不要**从 `3ce72bab` 开始，否则会漏掉分类终点之后、但日期早于最新 cherry-pick 的未吸收提交。先排除 JSON 里的 `backport` / `supplemental_backports` / `post_audit_backports` 以及本地已有的 `Zed-Origin` trailer。不要只按日期判断是否已同步。

#15–#19 共 cherry-pick 16 个上游 SHA（完整 hash 在 JSON `post_audit_backports`）：

| PR | 主题 | Zed SHA（短） |
|---|---|---|
| #15 | Linux X11/Wayland | `a49de953` `5dd0666d` `4d1935b8` `c43e2d97` `d9ad6aff` `f4178619` |
| #16 | Windows DPI / COLR | `1d7e5f1d` `7040aa56` |
| #17 | layout / SVG / test | `b1a7ef0c` `ff9f114c` `03c9c4e7` `7bddd16a` `3ce72bab` `eb548352` |
| #18 | HoverListenerMode | `f0d8b0b0` |
| #19 | util shell | `ce48461e` |

自 `4bd19937` 到对照 HEAD，映射路径上约有 79 个提交尚未做逐条分类；其中只有上表 16 个已吸收。2026-08-10 分类里仍成立的延期：外部文件拖放（`f52fd9ac` / `a8491e63` / `c7aea6cb`）、native flags（`e99616cd`）、Windows `path` crate（`26103320`）、同步动画（`4ed3738c`）。`79cc17c2` sticky-axis 滚动当时标为 deferred，已在独立 PR #5 吸收，不改写那次分类记录。

## 当前结构

```text
fc-gpui (public facade) -> fc-gpui-core
fc-gpui (public facade) -> fc-gpui-platform
fc-gpui-platform -> fc-gpui-core
fc-gpui-platform -> fc-gpui-macos -> fc-gpui-core
fc-gpui-platform -> fc-gpui-windows -> fc-gpui-core
fc-gpui-platform -> fc-gpui-linux -> fc-gpui-core
fc-gpui-linux -> fc-gpui-wgpu -> fc-gpui-core
```

`fc-gpui` 负责兼容现有下游入口和 `Application::new/headless`。核心状态、元素、布局、窗口协议和单元测试属于 `fc-gpui-core`。平台选择属于 `fc-gpui-platform`，具体操作系统实现不再位于 core。

## 上游路径映射

| Zed 路径 | 当前路径 | 说明 |
|---|---|---|
| `crates/gpui` | `crates/gpui` | 核心实现；本地 package 是 `fc-gpui-core` |
| 无直接对应 | `crates/gpui-compat` | 公共兼容入口、示例和集成测试 |
| `crates/gpui_platform` | `crates/gpui-platform` | 当前平台选择和应用组装 |
| `crates/gpui_wgpu` | `crates/gpui-wgpu` | Linux WGPU renderer |
| `crates/gpui_linux` | `crates/gpui-linux` | Linux/FreeBSD；主要实现位于 `src/linux` |
| `crates/gpui_macos` | `crates/gpui-macos` | 上游平铺文件对应本地 `src/mac` |
| `crates/gpui_windows` | `crates/gpui-windows` | 上游平铺文件对应本地 `src/windows` |
| `crates/gpui_macros` | `crates/gpui-macros` | proc macros；同时支持正常和重命名依赖 |
| `crates/collections` | `crates/collections` | `fc-gpui-collections` |
| `crates/util` | `crates/util` | `fc-gpui-util` |
| `crates/util_macros` | `crates/util_macros` | `fc-gpui-util-macros` |
| `crates/refineable` | `crates/refineable` | `fc-gpui-refineable` |
| `crates/sum_tree` | `crates/sum_tree` | `fc-gpui-sum-tree` |
| `crates/http_client` | `crates/http_client` | `fc-gpui-http-client` |
| `crates/media` | `crates/media` | `fc-gpui-media` |

不要直接覆盖 `app.rs`、`window.rs`、`platform.rs` 或平台 backend 文件。这些位置包含 daemon、tray、hotkey、overlay、notification、resource profile 和兼容层所需的本地行为。

## 当前明确分歧

| 主题 | 当前决定 | 重新评估触发条件 |
|---|---|---|
| Scheduler / queue | 保留本地 test-only priority prototype，不迁完整 scheduler | profiling 证明后台任务影响首帧、托盘弹窗或长期运行 |
| Structured system notifications | 延期；保留现有 title/body API | 应用需要 tag 替换、dismiss、action button 或点击回调 |
| Parent-anchored native popup | 延期 | Wayland/native popup 出现可复现的定位、焦点或关闭问题 |
| Web / mobile | 不在当前产品边界 | 项目明确增加非桌面平台目标 |
| View API / container query | `container_query` 与 sticky-axis 滚动已吸收；完整 View/ViewElement 重构仍独立评估 | 下游需要 View 重构或上游修复强依赖它 |
| Adabraka desktop extensions | 保留 | 只有兼容迁移方案和下游弃用计划同时存在时才可删除 |

这些是有意分歧，不应在增量审计中自动定为遗漏。

## 同步流程

1. 更新 `../zed` 并记录准备对照的上游 HEAD（写入 JSON `compared_against` 时用实际 fetch 到的 SHA）。
2. 只列出上一完整分类终点 `audited_upstream` 之后、影响映射 crate 的提交，并跳过已有 `Zed-Origin` / `post_audit_backports`。
3. 对每个提交标记：`backport`、`equivalent`、`deferred` 或 `not-applicable`。
4. 一个上游主题对应一个本地提交；backport 提交正文写完整 `Zed-Origin: <hash>`。
5. 先运行改动区域测试，再运行完整迁移验证。
6. 连续区间内**全部**提交分类完成后，才把 `audited_upstream` 和本文的分类终点前移。主题 cherry-pick 只追加 `post_audit_backports`，不要把未分类的 Zed HEAD 写成“已完全吸收”。

推荐的增量查询（下一轮从上一完整分类终点开始）：

```sh
git -C ../zed log --oneline \
  4bd1993783703e92affb781503916d1f152f599f..HEAD -- \
  crates/gpui crates/gpui_platform crates/gpui_wgpu \
  crates/gpui_linux crates/gpui_macos crates/gpui_windows crates/gpui_macros \
  crates/gpui_shared_string crates/gpui_tokio crates/gpui_util \
  crates/collections crates/util crates/util_macros crates/refineable \
  crates/sum_tree crates/http_client crates/media
```

## 有效验证命令

核心 focused test 必须指定 `fc-gpui-core`：

```sh
cargo test --locked -p fc-gpui-core --lib --features test-support -- profiler
cargo test --locked -p fc-gpui-core --lib --features test-support -- elements::list
```

公共兼容入口单独验证：

```sh
cargo test --locked -p fc-gpui --tests --features test-support
cargo check --locked -p fc-gpui-downstream-compat
cargo check --locked -p fc-gpui-renamed-dependency-compat
```

完整本地闭环：

```sh
scripts/verify-migration.sh
```

发布前安装 `cargo-semver-checks` 后执行：

```sh
scripts/verify-migration.sh --semver
```

Linux/Windows 的真实窗口、输入、托盘和 GPU 行为仍由对应平台 CI 或人工 smoke test 判定；交叉编译不能替代运行时证据。

## 历史材料

- `ZED_UPSTREAM_MIGRATION_REVIEW_2026-08-10.md`：上一轮完整分类（到 `4bd19937`）的价值审计；不要改写其中的当时结论。
- `ZED_GPUI_INCREMENTAL_AUDIT_2026-07-01.md`：带日期的增量判断和 backport 记录。
- `ZED_GPUI_P1_BACKPORT_2026-07-20.md`：P1 批次及当时的平台验证证据。
- `ZED_SYNC_MAPPING.md`、`QUICK_SYNC_REFERENCE.md`、`TECHNICAL_SYNC_GUIDE.md`：拆分前指南，仅用于历史追溯。

更新历史材料中的旧命令不会改变当时的事实，因此不要批量重写；当前执行以本文为准。
