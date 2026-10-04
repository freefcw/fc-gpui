# GPUI 上游架构能力迁移 Todo

日期：2026-05-17

本目录记录 Zed 上游 GPUI 架构能力的迁移评估、拆分任务和验证方案。

## 总入口

- `gpui-upstream-architecture-migration-evaluation.md`：总体评估、收益、风险、阶段计划。

## 任务列表

| 顺序 | 任务 | 文档 | 当前状态 | 依赖 | 建议优先级 |
| --- | --- | --- | --- | --- | --- |
| 1 | Profiler / Observability | `001-profiler-observability.md` | 已完成首版 | 无 | 最高 |
| 2 | TestApp / Headless | `002-test-app-headless.md` | 已完成首版 | 可独立，建议在 profiler 后 | 高 |
| 3 | Visual Test Platform | `003-visual-test-platform.md` | 已完成首版（能力探测、mock artifact、macOS real smoke/screenshot readback） | TestDispatcher deterministic clock 能力稳定 | 中 |
| 4 | Scheduler / Queue | `004-scheduler-queue.md` | 已完成本地 priority 首版 | profiler + test 基线 | 最高风险，最后做 |

## 执行原则

- 不按上游文件直接覆盖。
- 不先替换 `Task` / executor。
- 不先做大规模 pixel snapshot。
- 每个任务独立提交，独立验证。
- 任何平台 dispatcher、executor、window lifecycle 改动都必须保留 Adabraka 的 daemon、tray、headless、resource profile 能力。

> **架构说明（Multi-Crates Workspace）**：项目已拆分为 Workspace 多 Crate 架构（`fc-gpui-core` (`crates/gpui`)、`fc-gpui` (`crates/gpui-compat`)、`fc-gpui-wgpu` 等）。单元测试已归属至 `fc-gpui-core`。下述命令中的单元测试与检查均以 `fc-gpui-core` 为准，推荐使用 `scripts/verify-001.sh` ~ `scripts/verify-004.sh` 统一验证。

## 统一开关策略

| 能力 | 策略 |
| --- | --- |
| Profiler | 始终编译，`AtomicBool` 运行时开关，默认关闭 |
| TestApp / HeadlessTestApp | `#[cfg(any(test, feature = "test-support"))]` |
| Visual Test | `#[cfg(any(test, feature = "test-support"))]` + 运行时能力探测 |
| Priority API | 始终编译，默认 Medium，opt-in 使用 |

## 推荐执行路径

### 第一批：先拿观测能力

执行：

- `001-profiler-observability.md`

目标：

- 采集 task timing。
- 支持增量 collector。
- 默认关闭，低风险接入。

最小验证：

```bash
cargo test -p fc-gpui-core profiler
cargo test -p fc-gpui-core --lib --features test-support profiler
cargo check -p fc-gpui-core --no-default-features
cargo check -p fc-gpui-wgpu
```

### 第二批：改善测试入口和 headless 能力

执行：

- `002-test-app-headless.md`

目标：

- 提供 `TestApp` wrapper。
- 自动 flush effects。
- 支持 headless text shaping 测试。

最小验证：

```bash
cargo test -p fc-gpui-core test_app
cargo test -p fc-gpui-core headless
cargo test -p fc-gpui-core --lib --features test-support
```

### 第三批：真实渲染 smoke

执行：

- `003-visual-test-platform.md`

目标：

- 增加 visual test capability detection。
- 支持真实 renderer smoke。
- unsupported 环境自动 skip。

最小验证：

```bash
cargo test -p fc-gpui-core visual_test --features test-support
cargo test -p fc-gpui-core --lib --features test-support
```

平台手动验证：

```bash
cargo test -p fc-gpui-core real_visual --features test-support -- --ignored
```

### 第四批：调度和优先级

执行：

- `004-scheduler-queue.md`

目标：

- 先加本地 `TaskPriority`。
- 引入 dispatcher adapter priority。
- 改造 test dispatcher priority queue。
- 最后再评估是否引入上游 `scheduler` crate。

最小验证：

```bash
cargo test -p fc-gpui-core executor
cargo test -p fc-gpui-core dispatcher
cargo test -p fc-gpui-core --lib --features test-support
cargo check -p fc-gpui-wgpu
```

## 验证分层策略

验证分为三层，按场景选用：

### 层 1：编译确认（每个 Step 完成后必跑）

```bash
cargo check -p fc-gpui-core
cargo check -p fc-gpui-core --no-default-features
cargo check -p fc-gpui-wgpu
```

确保改动不破坏任何 feature 组合的编译。三条命令分别覆盖默认 macOS 路径、最小编译、Linux wgpu 路径。

### 层 2：全量 lib 测试（每个任务开始前/完成后必跑）

```bash
cargo test -p fc-gpui-core --lib --features test-support
```

当前共 116+ 个 lib 测试，涵盖 app、executor、keymap、elements、text_system、platform 等模块。这是回退检测的主要手段。

### 层 3：平台特定和真实渲染（涉及平台改动时）

```bash
# Linux
cargo test -p fc-gpui-linux --lib --features test-support,wayland,x11

# macOS 真实渲染（标记为 ignored 的测试）
cargo test -p fc-gpui-core --lib --features test-support -- --ignored

# 快速 smoke（不替代全量测试，适合频繁确认）
cargo test -p fc-gpui-core window_positioner
```

### 冻结测试清单

以下测试模块在所有迁移任务期间必须始终通过。如果某个 step 导致其中任何一个失败，应立即停止并修复：

```bash
cargo test -p fc-gpui-core --lib --features test-support -- app::test
cargo test -p fc-gpui-core --lib --features test-support -- executor
cargo test -p fc-gpui-core --lib --features test-support -- elements::list
cargo test -p fc-gpui-core --lib --features test-support -- keymap
cargo test -p fc-gpui-core --lib --features test-support -- text_system
cargo test -p fc-gpui-core --test action_macros
```

这些测试覆盖核心功能（app 生命周期、task 调度、UI 元素、按键绑定、文本排版），是判断改动是否引入回退的最低门槛。

### 本地验证脚本约定

每个任务配一个验证脚本 `scripts/verify-NNN.sh`，包含该任务所有层验证命令。脚本结构：

```bash
#!/bin/bash
set -e
echo "=== NNN - Task Name ==="
echo "[1/N] Compile checks..."
echo "[2/N] New feature tests..."
echo "[3/N] Frozen tests (regression guard)..."
echo "[4/N] Full lib test..."
echo "=== ALL PASSED ==="
```

### 并发测试策略

利用 `#[gpui::test]` 宏的 `iterations` 参数发现竞态问题。对涉及共享状态的新功能（如 profiler 的线程安全、priority queue 的并发访问），测试应指定多次迭代：

```rust
#[gpui::test(iterations = 50)]
async fn concurrent_feature_is_stable(cx: &mut TestAppContext) { ... }
```

环境变量 `SEED` 可用于复现特定失败种子。

## 状态维护约定

每完成一个任务：

1. 在对应任务文档更新“当前状态”。
2. 补充实际验证命令和结果。
3. 若发现计划偏差，在“后续扩展”或“风险”里追加。
4. 每个任务单独提交，避免和实现无关变更混在一起。

## 已知迁移债务

以下问题不在当前四个任务的直接范围内，但实施时需要意识到它们的存在：

| 债务 | 说明 | 触发时机 |
| --- | --- | --- |
| `Instant` 类型 | 当前用 `std::time::Instant`，上游 profiler 用 `scheduler::Instant` | 若后续引入 scheduler，需全局替换 |
| `Task<T>` 类型 | 当前基于 `async_task::Task<T>`，上游已替换为 `scheduler::Task<T>` | 引入 scheduler 时所有 Task 使用者受影响 |
| `PlatformDispatcher` 接口 | 上游已增加 `Priority` 和 `RunnableMeta` 参数 | 引入 scheduler 时需修改 trait 或增加适配层 |
| `profiling` crate vs `profiler.rs` | 前者是编译期函数级 tracing（Tracy 等后端），后者是运行时 task-level timing 采集，两者互补共存 | 实施 001 时需在文档中说明两者关系 |

## 已知依赖债务

| 债务 | 说明 | 触发时机 |
| --- | --- | --- |
| objc2 双版本 | `accesskit_macos`（截至 0.27.1）仍依赖 objc2 0.5 系，与 `gpui-macos` 的 objc2 0.6 系并存；升级 accesskit 系还需连带迁移 accesskit 0.25 API | 等上游 accesskit 迁移到 objc2 0.6 后统一 |

## 当前建议

四个首版迁移任务均已完成。后续工作应按“已知迁移债务”和各任务文档中的“后续扩展”单独拆分，例如完整 scheduler crate 引入、跨平台 screenshot readback、golden snapshot 阈值工具或 profiler overlay。
