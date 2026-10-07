# Adabraka GPUI vs Zed GPUI 功能对比

对比 Adabraka GPUI 与上游 Zed GPUI 的功能差异。

## 完全新增的功能（Zed 不存在）

### 1. 守护进程模式 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `set_quit_mode(QuitMode::Explicit)` - 无窗口保活（原 `set_keep_alive_without_windows()`，已改为语义化 `QuitMode`）
- 应用可在没有可见窗口时继续运行
- 适用于后台应用、菜单栏工具

**实现**:
- macOS: `applicationShouldTerminateAfterLastWindowClosed` 委托
- Linux/Windows: 事件循环标志控制

### 2. 系统托盘 (v0.4.0)
**状态**: ✅ Adabraka 独有

完整的系统托盘 API:
- `set_tray_icon()` / `set_tray_tooltip()`
- `set_tray_menu()` - 嵌套菜单支持
- `on_tray_menu_action()` - 菜单动作回调
- `on_tray_icon_event()` - 图标点击事件（v0.11 起 macOS 区分左/右/双击）
- `on_tray_icon_click_event()` - 带位置 hint 的点击事件
- `set_tray_panel_mode()` - 面板模式（v0.4.1；v0.11 起语义收窄，见下）
- `get_tray_icon_bounds()` / `tray_icon_anchor()` - 位置锚点
- `set_tray_icon_rendering_mode()` - 渲染模式控制（v0.6.0）

**实现**:
- macOS: NSStatusBar/NSStatusItem
- Linux: ksni (DBus StatusNotifierItem)
- Windows: Shell_NotifyIconW

**macOS 点击语义（v0.11 起）**:
- 点击 handler 在创建 tray 时即 wire 到 status item button，无菜单附着时注册回调即收到点击，
  不再需要先调 `set_tray_panel_mode(true)` 这个隐性前置条件。
- `set_tray_panel_mode(true)` 的语义收窄为"detach 已附着的菜单，让点击事件优先"——只在
  "既要菜单又要点击"的场景需要显式调用。
- 点击类型按 `NSApp.currentEvent` 区分：左键 `LeftClick`、右键 `RightClick`、左键双击
  `DoubleClick`；程序化 `performClick:` 回落为 `LeftClick`。
- "注册了点击回调 + 附着菜单 + 未开 panel mode"是歧义组合（AppKit 中菜单会吞掉点击），
  两种注册顺序下各触发一次 `log::warn`。
- 回调在点击后的下一个主队列 turn 派发（非同步），tray 弹窗的首帧在同 turn 内完成。
- macOS 不提供点击坐标（`TrayIconClickEvent.position` 为 `None`），定位请用
  `tray_icon_bounds()`。

**Linux 定位限制**:
- StatusNotifierItem 的 `activate(x, y)` 坐标是托盘宿主提供的屏幕坐标 hint，不是图标真实 bounds。
- GPUI 会把 Linux 原始坐标按显示器 scale 转换为逻辑 `Pixels`，再用于 `TrayAnchor` 近似定位。
- Wayland fractional scaling 下，SNI hint 没有关联的 Wayland surface，只能使用 `wl_output.scale` 做近似转换，不能精确使用 per-surface fractional scale。
- 部分 AppIndicator 扩展宿主不派发 `Activate` 事件（左键点击无回调）。fallback 菜单是
  必须项而非可选项：下游应在收不到点击事件时保留菜单路径。

**平台行为与限制速查**（tray / 弹窗定位）:

| 能力 | macOS | Linux X11 | Linux Wayland |
| --- | --- | --- | --- |
| 图标真实 bounds | ✅ `tray_icon_bounds()` | ❌ 只有 SNI 坐标 hint | ❌ 只有 hint（近似） |
| 左/右键区分 | ✅ | ✅（SNI activate / secondary_activate） | 取决于宿主 |
| 双击事件 | ✅ | ❌ | ❌ |
| 弹窗全局定位 | ✅ | ✅ | ❌（见窗口定位器一节） |

### 3. 全局热键 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `register_global_hotkey()` - 注册系统级快捷键
- `unregister_global_hotkey()` - 注销快捷键
- 键名规范化（v0.4.1）

**实现**:
- macOS: Carbon RegisterEventHotKey（v0.6.0 从 NSEvent 迁移）
- Linux X11: XGrabKey
- Windows: RegisterHotKey
- Linux Wayland: 不支持（协议限制）

### 4. 覆盖窗口 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `WindowKind::Overlay` - 始终置顶的窗口类型
- `set_mouse_passthrough()` - 鼠标穿透（点击穿透）
- 适用于屏幕覆盖、HUD、悬浮工具

**实现**:
- macOS: window level 25, stationary, all spaces
- Linux X11: shape extension
- Linux Wayland: wl_region
- Windows: HWND_TOPMOST + WS_EX_TRANSPARENT

**对比**: Zed 只有 `Normal`, `PopUp`, `Floating`

### 5. 原生通知 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `show_notification()` - OS 级通知
- Toast 组件 - 应用内通知（可堆叠、自动消失；已废弃，待移除——组件层职责，请改用 fc-ui 的 toast 组件）

**实现**:
- macOS: UNUserNotificationCenter
- Linux: notify-rust
- Windows: Shell balloon

### 6. 窗口显示控制 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `show()` / `hide()` / `is_visible()` - 窗口可见性控制
- 所有平台实现
- `WindowOptions::show` / `focus` 三端一致消费（v0.11 起）：macOS 在建窗时
  `makeKeyAndOrderFront`/`orderFront`；Linux X11 由 facade 按 `show` 决定是否 map、
  map 后按 `focus` 请求 `_NET_ACTIVE_WINDOW`；Linux Wayland 的 `show` 仍受协议限制
  （首次 commit 即可见，暂等价于 `show: true`）。

### 6a. 锚定无动画 resize (v0.11)
**状态**: ✅ Adabraka 独有

- `Window::resize_anchored(size, anchor)` / `PlatformWindow::resize_anchored`
- `ResizeAnchor` 九宫格枚举（四角 + 四边 + 中心）：resize 时保持锚定的角/边在屏幕上不动。
- macOS 实现为同步 `setFrame:display:animate:NO`（无系统 resize 动画，调用返回时已生效）；
  配套修复了 resize/moved 回调在 window update 内同步触发时的重入丢失问题。
- Linux/Windows 目前回落到 `resize`（不锚定），需要锚定的下游应自行按
  `tray_icon_bounds()` 计算或等待后续实现。
- 典型场景：菜单栏 tray 弹窗用 `ResizeAnchor::TopLeft`，向下展开而不脱离图标。

### 7. 自动启动 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `enable_auto_launch()` / `disable_auto_launch()`
- `is_auto_launch_enabled()`

**实现**:
- macOS: SMAppService (macOS 13+)
- Linux: XDG autostart desktop 文件
- Windows: Registry Run 键

### 8. 单实例锁 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `try_acquire_single_instance_lock()` - 防止多实例
- 激活信号传递

**实现**:
- macOS/Linux: Unix domain socket
- Windows: Named mutex

### 9. 焦点窗口信息 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `get_focused_window_info()` - 查询用户当前焦点窗口

**实现**:
- macOS: NSWorkspace + Accessibility API
- Linux X11: EWMH
- Windows: Win32

### 10. 权限查询 (v0.4.0)
**状态**: ✅ Adabraka 独有

- `check_accessibility_permission()`
- `check_microphone_permission()`

**实现**: macOS only (AXIsProcessTrusted)

### 11. 电源事件 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `on_power_event()` - 系统电源状态变化回调

**实现**:
- macOS: NSWorkspace 通知观察者
- Windows: WM_POWERBROADCAST, WM_WTSSESSION_CHANGE
- Linux: 存根

### 12. 网络状态 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `get_network_status()` / `on_network_status_change()`

**实现**:
- macOS: NWPathMonitor (Network framework)
- Windows: INetworkListManager COM
- Linux: 读取 /sys/class/net/*/operstate

### 13. 媒体键 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `on_media_key()` - 播放/暂停/下一曲等媒体键回调

**实现**:
- macOS: NSEvent global monitor (NSSystemDefinedMask)
- Windows: WM_APPCOMMAND
- Linux: XF86 keysym 拦截

### 14. 原生对话框 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `show_dialog()` - 原生消息框

**实现**:
- macOS: NSAlert
- Windows: MessageBoxW
- Linux: zenity + kdialog 回退

### 15. 进度条 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `set_progress_bar()` - 任务栏/Dock 进度显示

**实现**:
- macOS: NSDockTile + NSProgressIndicator
- Windows: ITaskbarList3
- Linux: 存根

### 16. 用户注意力请求 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `request_user_attention()` - 窗口闪烁/弹跳

**实现**:
- Windows: FlashWindowEx
- Linux X11: EWMH _NET_WM_STATE_DEMANDS_ATTENTION
- macOS: 存根

### 17. 系统空闲时间 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `get_idle_time()` - 获取用户无操作时长

**实现**:
- Windows: GetLastInputInfo
- Linux X11: screensaver extension
- macOS: 存根

### 18. 省电阻止器 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `prevent_sleep()` / `allow_sleep()` - 阻止系统休眠

**实现**:
- macOS: PreventUserIdleDisplaySleep
- Windows: SetThreadExecutionState
- Linux: dbus-send screensaver inhibit, systemd-inhibit

### 19. 上下文菜单 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `show_context_menu()` - 原生右键菜单

**实现**:
- macOS: NSMenu + handleContextMenuItem
- Windows: TrackPopupMenu
- Linux: 存根

### 20. 系统信息 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `get_os_info()` - OS 名称、版本、主机名等

**实现**:
- macOS: 系统 API
- Windows: RtlGetVersion
- Linux: 解析 /etc/os-release, /etc/hostname

### 21. 生物识别认证 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `authenticate_biometric()` - Touch ID / Windows Hello

**实现**: macOS only (LocalAuthentication framework)

### 22. 窗口定位器 (v0.5.0)
**状态**: ✅ Adabraka 独有

- `WindowPosition` 枚举：Center, CenterOnScreen, TrayCenter, TrayAnchored, Custom
- 简化窗口定位逻辑

**Wayland 平台限制（协议层面，短期不改）**:
- xdg_toplevel 的位置由 compositor 决定，客户端没有全局定位协议；`Custom` /
  `TrayAnchored` 在 Wayland 上无法精确生效，弹窗常被 compositor 重映射到屏幕中央。
- 这不是实现缺陷：下游必须把"fallback 到居中/角落 + 透明占位窗口"当作 Wayland 的
  正常形态来设计，而不是可选优化。X11 与 macOS 不受此限制。

## 渲染增强（Adabraka 独有）

### 1. 元素变换 (v0.3.0)
**状态**: ⚠️ 部分存在

Zed 有 `TransformationMatrix` 结构体，但 **没有** Styled trait 的便捷方法：
- ❌ `.rotate()` - Adabraka 独有
- ❌ `.scale()` / `.scale_xy()` - Adabraka 独有
- ❌ `.transform_origin()` - Adabraka 独有

Zed 只在内部使用 TransformationMatrix，未暴露给用户 API。

### 2. 多色渐变 (v0.3.0)
**状态**: ✅ Adabraka 独有

- ❌ 4 色停止点（Zed 只有 2 色）
- ❌ `RadialGradient` - Adabraka 独有
- ❌ `ConicGradient` - Adabraka 独有

### 3. 混合模式 (v0.3.0)
**状态**: ✅ Adabraka 独有

- ❌ `BlendMode` 枚举 - Adabraka 独有
- ❌ Multiply, Screen, Overlay, SoftLight, Difference - Adabraka 独有

### 4. 元素调整事件 (v0.3.0)
**状态**: ✅ Adabraka 独有

- ❌ `on_resize()` - Adabraka 独有
- ❌ `ResizeEvent` - Adabraka 独有

## 从 Zed 同步的优化

### 1. SharedString 优化 (v0.6.2)
**状态**: ✅ 已同步

- ✅ 从 ArcCow 迁移到 smol_str
- ✅ 结构体大小从 32 字节减少到 24 字节
- ✅ 小字符串优化（< 23 字节内联）

**来源**: Zed commit 58d3a9eef4

### 2. X11 窗口图标 (v0.6.2)
**状态**: ✅ 已同步

- ✅ `set_window_icon()` - 通过 _NET_WM_ICON

**来源**: Zed commit 24a304c140

### 3. macOS 字形膨胀 (v0.6.2)
**状态**: ⚠️ 简化版本

- ✅ 基于亮度的字形膨胀（2 级：暗/亮）
- ⚠️ Zed 使用 5 级，Adabraka 简化为 2 级

**来源**: Zed commit a38fc8c8de

### 4. 输入模式跟踪 (v0.6.2)
**状态**: ✅ 已同步

- ✅ `last_input_was_keyboard()` - 跟踪键盘/鼠标输入

**来源**: Zed commit d010b06a77

### 5. Bug 修复
**状态**: ✅ 已同步

- ✅ 锚定元素负坐标大小计算（Zed b38194198b）
- ✅ GIF 渲染越界 panic（Zed 749fcfdfd8）
- ✅ 窗口失焦时恢复光标（Zed c01671eac1）
- ✅ 移除 naga 构建依赖（Zed e712f3c6df）

### 6. Layer-Shell 架构演进与 App 资源配置 (v0.7.0)
**状态**: ✅ Adabraka 独有 / 架构增强

- ✅ **规范化 Layer-Shell API**：采用 `WindowKind::LayerShell(LayerShellOptions)` 显式选择窗口类型，移除旧版隐式 Overlay 行为，提供 `wlr-layer-shell` 运行时校验与错误响应。
- ✅ **应用资源配置文件 (`AppProfile`)**：提供 `Desktop`, `Utility`, `Minimal` 及 `Custom` 预设，运行时控制动态 Atlas 分配与 Line-layout 缓存水印淘汰（Watermark Eviction）。
- ✅ **Linux WGPU Quad/Background ABI 对齐**：消除多 stop 渐变在 Linux 平台上的渲染偏差与 NaN 崩溃。

### 7. 多 Crate 架构拆分与全平台契约硬化 (v0.8.0 / v0.8.1)
**状态**: ✅ Adabraka 独有 / 架构重构

- ✅ **Workspace 8 包拆分架构**：解耦核心引擎与底层平台包（`fc-gpui-core`, `gpui-wgpu`, `gpui-linux`, `gpui-macos`, `gpui-windows`, `gpui-platform`, `gpui-macros` 及Facade `fc-gpui`）。
- ✅ **全平台权限与 Accessibility 契约**：引入 `PermissionStatus::Unavailable` 和 `PermissionRequestStatus`，支持键盘快捷键辅助树节点探测与自动化视觉测试产物。
- ✅ **屏幕捕获生命周期**：增加流创建 `Ended` / `Cancelled` / `Failed` 回调机制，释放 macOS 捕获句柄。
- ✅ **Windows DirectX 离屏 Readback 物理对齐 (v0.8.1)**：对齐 HLSL Quad Transform Padding 物理结构。

## 平台现代化（Adabraka 独有）

### macOS objc2 迁移 (v0.6.0)
**状态**: ✅ Adabraka 独有

- 从弃用的 cocoa crate 迁移到 objc2-app-kit/objc2-foundation
- 消除所有 cocoa 弃用警告
- Zed 仍在使用 cocoa crate

### 图像格式可选功能 (v0.6.0 / v0.8.0)
**状态**: ✅ Adabraka 独有

- `image-format-*` 功能标志，默认精简包含 GIF, JPEG, PNG, WebP
- 允许下游按需选择高级解码器（AVIF, EXR, QOI, HDR 等）控制二进制体积

## 性能优化对比

### Adabraka 独有优化

1. **场景图排序与去重优化** (v0.5.1 / v0.8.0)
   - `is_sorted_by_key` 检查跳过已排序数据
   - 避免无边框 Quad 过度绘制 (Overdraw)
   - GPU 缓冲区 1.5x 增长策略与动态预算控制

2. **DirectX 与 macOS 线程调度优化** (v0.5.1 / v0.8.0)
   - DirectWrite 文本格式缓存与管线状态缓存
   - 跨窗口文本布局缓存
   - 使用 Win32 线程池与 macOS DisplayLink 共享调度
   - Taffy 布局引擎升级至 0.12.2

### 从 Zed 同步的优化

1. **SharedString smol_str** (v0.6.2) - 已同步

## 总结

### Adabraka 独有功能统计

- **桌面应用功能**: 26+ 项（守护进程、托盘、热键、通知、电源管理、原生对话框、屏捕生命周期、权限状态等）
- **渲染与资源管理**: 7 项（变换、多 Stop 渐变、混合模式、Resource Profiles、Layer-shell 规范、WGPU 资源预算、DX/WGPU Readback）
- **平台现代化与架构**: 4 项（objc2 迁移、图像格式按需选择、Workspace 8 Crate 拆分解耦、宏解析适配器）
- **性能优化**: 5 项（场景图免排序、无边框 Quad 过载优化、Win32 线程池调度、DisplayLink 共享、DirectX/TextSystem 缓存）

**总计**: 42+ 项主要功能与架构演进为 Adabraka 独有

### Zed 同步功能统计

- **已同步优化**: 5 项（SharedString、窗口图标、字形膨胀、输入跟踪、bug 修复）

### 定位差异

- **Zed GPUI**: 专注于 Zed 编辑器应用的单 Crate 内部 UI 框架
- **Adabraka GPUI**: 模块化解耦、面向全平台原生生态与桌面应用的轻量通用 GUI 框架

Adabraka 是 Zed GPUI 的超集与模块化升级版，保持与上游核心兼容的同时提供了完整的平台能力与资源管理体系。
