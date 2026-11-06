# Windows 中文 IME 验证

**状态：待实测。** 当前开发机是 macOS，暂无可用 Windows 测试机。
已通过的坐标转换单元测试不代表 Windows 候选窗定位通过；普通视觉
smoke 测试也不覆盖真实中文输入法交互。

## 环境准备

- Windows 10/11 交互式桌面，安装 Microsoft 拼音，记录系统版本、IME
  版本及是否开启旧版兼容模式。其他输入法单独记录，不外推结果。
- 安装仓库 `rust-toolchain.toml` 指定的 Rust、Visual Studio C++ Build
  Tools 和 Windows SDK。
- 至少分别验证 100%、125%、150%、200% 显示缩放；如有条件，再使用
  不同 DPI 的双显示器。没有双屏时将跨屏项保留为待实测。
- 优先使用本地桌面。若通过远程桌面操作，记录客户端、会话分辨率及
  输入法运行在本地还是远端；远程结果不能替代本地多 DPI 验证。

在仓库根目录的 PowerShell 中运行：

```powershell
git rev-parse HEAD
git status --short
rustc -Vv
cargo test -p fc-gpui-windows --lib --locked
cargo run -p fc-gpui --example input --locked
```

保存提交号和未提交差异，确保测试覆盖实际待发布代码。示例见
[`input.rs`](../crates/gpui-compat/examples/input.rs)。该示例为单行输入框，
可验证基本输入，但没有滚动容器，也没有显式调用
`Window::invalidate_character_coordinates`，不能单独验收后续坐标更新。

## 基础交互步骤

| 场景 | 操作 | 通过条件 | 状态 |
| --- | --- | --- | --- |
| 开始组合输入 | 点击输入框，切换中文，输入 `zhongwen`，暂不提交 | 预编辑文字可见，候选窗靠近当前输入位置，无跳至屏幕原点 | 待实测 |
| 提交及取消 | 选候选词提交，再次输入并按 Esc 取消 | 提交只发生一次，取消不残留预编辑文字或候选窗 | 待实测 |
| 改变插入位置 | 提交文本，用 Home/End、方向键或鼠标改变 caret，再开始输入 | 候选窗定位到新位置，而非上一次输入位置 | 待实测 |
| 焦点切换 | 输入期间切换至其他应用再返回 | 无崩溃、死锁或遗留候选窗；恢复输入时位置正确 | 待实测 |
| 移动窗口 | 开始输入后移动窗口，再输入 | 候选窗跟随窗口；若 IME 自动提交或取消，记录该行为并重新开始输入验证 | 待实测 |
| 缩放 | 在每个指定 DPI 下重新启动示例并重复上述步骤 | 无随缩放增加的定位偏移 | 待实测 |
| 跨屏 | 将窗口从一个 DPI 的显示器移动到另一个，再开始输入 | 新显示器上的位置正确，无旧 scale factor 遗留 | 待实测 |

候选窗可能由系统为避开屏幕边缘而翻转，不要求固定绝对像素位置；
应确认它始终锚定当前输入位置且不出现持续漂移。

## 后续坐标更新与滚动

使用包含可滚动文本编辑器的应用或专用测试界面。它必须注册
`ElementInputHandler`，并在 caret 或布局位置改变后调用
`Window::invalidate_character_coordinates()`。没有这样的测试界面时，
以下项目继续保持待实测，不能用单行示例代替。

1. 保持组合输入有效，移动 caret 或改变输入框布局，再请求坐标失效。
2. 在包含输入框的容器内滚动，使 caret 在窗口客户区中的位置改变。
3. 在 Windows 调试器中对 `WindowsWindow::update_ime_position` 设置断点，
   确认没有重启组合输入也能进入该路径；检查传入 bounds 与最新 caret
   一致，并采用当前窗口的 scale factor。观察候选窗是否随后跟随。
4. 连续滚动、调整大小及切换 DPI，确认没有定位停留在旧 bounds、崩溃
   或 UI 卡死。分别记录每项结果。

对应实现：[`Window::invalidate_character_coordinates`](../crates/gpui/src/window.rs)、
[`WindowsWindow::update_ime_position`](../crates/gpui-windows/src/windows/window.rs)、
[`set_ime_position`](../crates/gpui-windows/src/windows/events.rs)、
[`physical_ime_position`](../crates/gpui-windows/src/ime_position.rs)。

## 结果记录

每次实测复制以下表格。所有必测项有结果后才能标记通过；未运行项
保留“待实测”，不以单元测试通过替代。

| 字段 | 记录 |
| --- | --- |
| 日期、执行人 | 待填写 |
| 提交号、未提交差异 | 待填写 |
| Windows、IME 版本与兼容模式 | 待填写 |
| GPU、驱动、显示器 DPI、远程会话信息 | 待填写 |
| 测试应用与启动命令 | 待填写 |
| 基础交互、各 DPI、跨屏结果 | 待实测 |
| 坐标更新回调、滚动结果 | 待实测 |
| 录屏、日志、失败复现步骤 | 待填写 |

发生失败时记录输入顺序、缩放比例和失效前后的 caret/候选窗位置，
保留日志与录屏；修复后复跑失败场景及基本提交/取消流程。
