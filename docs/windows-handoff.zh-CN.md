# 交接文档：iphone-use Windows 原生支持

> 状态日期：2026-10-08 · 分支 `claude/clever-allen-5lmh8e`（已合并到 fork 的 `main`）
> 相关：[PR #1](https://github.com/Souitou-iop/iphone-use/pull/1) · 使用说明 [windows.zh-CN.md](windows.zh-CN.md)

## 1. 一句话现状

守护进程 `iphone-use.exe` 和 MCP 桥 `iphone-use-mcp.exe` 已经能在 Windows x64 上**原生编译和运行**，CI 全绿。
**还没在真机上验证**的是：go-ios 启动手机上的 runner → 实际点击、读屏这一段。下一步就是在 Windows 实机上调这一段。

## 2. 整体架构（Windows 版）

```
 Agent / 浏览器
      │  HTTP :44321 (/agent/*, /phone)      MCP stdio
      ▼                                        ▼
 iphone-use.exe serve  ◄────────────── iphone-use-mcp.exe
      │  http://127.0.0.1:8100 (控制)  :9100 (MJPEG)
      ▼
 iphone-use.exe relay ×2 ──► Apple Mobile Device Service (usbmuxd, TCP 127.0.0.1:27015)
                                   │ USB
                                   ▼
                       iPhone 上的 iPhoneUse-Runner（XCTest，监听 8100/9100）
                                   ▲
           go-ios: ios tunnel start --userspace（iOS 17+）/ ios image auto / ios runtest
```

和 macOS 的差别：

| 环节 | macOS | Windows |
|---|---|---|
| usbmuxd | `/var/run/usbmuxd`（Unix 套接字） | Apple Mobile Device Service，`127.0.0.1:27015`（TCP） |
| 编译、签名 runner | `setup-wda.sh` + Xcode | CI 产出未签名 IPA，用户用 Sideloadly 签 |
| 启动 runner | `xcodebuild test-without-building` | go-ios `ios runtest` |
| 保活 / 重启 runner | LaunchAgent + 守护进程托管 | 无；`PHONE_REMOTE_WDA_MANAGED` 在 Windows 默认关闭 |
| 实时画面 | H.264（VideoToolbox）或 MJPEG | 只有 MJPEG |

## 3. 改了哪些东西

### 代码（Rust）

| 文件 | 改动 |
|---|---|
| `crates/server/src/platform.rs`（新） | Unix 文件权限位、`O_NOFOLLOW`、uid、`localtime_r` 的跨平台封装；Windows 上是空操作或 `localtime_s`。`home_dir()` / `ensure_home()`：没有 `HOME` 时用 `USERPROFILE` |
| `crates/mcp/src/platform.rs`（新） | 同上的精简版（MCP crate 不依赖 server crate） |
| `crates/server/src/usbmux.rs` | 连接 usbmuxd 抽象成 `open_mux()` → `MuxStream`（`Box<dyn AsyncRead+AsyncWrite>`）。Windows 默认 TCP 27015；支持 `USBMUXD_SOCKET_ADDRESS`；Windows 上读配对记录失败时回退到 `%ProgramData%\Apple\Lockdown\<udid>.plist` |
| `crates/server/src/main.rs` | pid 记录与 `stop`：Windows 用 Win32（`OpenProcess` / `GetProcessTimes` / `QueryFullProcessImageNameW` / `TerminateProcess`）代替 `ps` / `kill`；`on_path`、打开浏览器（`cmd /C start`）、`launchctl` / `osascript` 只在 macOS 调；`stderr_is_tty` 改用 `std::io::IsTerminal`；Windows 默认 `PHONE_REMOTE_WDA_MANAGED=false` |
| `crates/server/src/runtime_dir.rs` | 运行目录在 Windows 用 `%TEMP%`；uid / mode 校验只在 Unix 做 |
| `crates/server/src/pairing.rs` | Windows 用 `if-addrs` 取局域网 IP（Unix 仍是 `getifaddrs`） |
| `crates/server/src/{instance,schedules,flows}.rs`、`crates/mcp/src/{flow,registry,outputs,suite,schedule}.rs` | 引用改到 `platform` 模块；`flows::mcp_binary()` 加 `.exe` 后缀 |
| `crates/server/Cargo.toml` | 仅 Windows 依赖：`if-addrs 0.13`、`windows-sys 0.61` |

设计原则：**macOS 行为不变**。所有 Windows 分支都用 `#[cfg(windows)]` 隔开，Unix 路径保持原逻辑。

### 脚本、CI、文档

- `scripts/windows/iphone-use.ps1`：一键启动（见第 5 节）。
- `.github/workflows/windows.yml`：
  - `Windows x64 binaries`（windows-latest）：编译 → 冒烟测试（`--version`、`instance-context`、`flow validate`、MCP `tools/list` 必须为 23 个）→ 打包 `iphone-use-windows-x64.zip`。
  - `Unsigned runner IPA`（macos-14）：`IPU_RUNNER_UNSIGNED=1 runner/build.sh` → `Payload/iPhoneUse-Runner.app` → `iPhoneUse-Runner-unsigned.ipa`。
  - 推 `v*` 标签时两者都会上传到 Release。
- `docs/windows.md`、`docs/windows.zh-CN.md`：用户使用说明；README 加了链接。

## 4. 已验证 / 未验证

已验证：
- Linux 上 `cargo test -p server` 全部通过；`iphone-use-mcp` 只有一个测试失败，原因是沙箱以 root 运行导致目录权限不生效，与本改动无关。
- 原有的 macOS `PR checks` 在 GitHub Actions 上通过（Mac 端没被改坏）。
- Windows CI（MSVC 工具链）编译和冒烟测试通过。
- 交叉编译出的 Windows exe 在 Wine 下，对接自写的假 usbmuxd（TCP 27015）和假 runner，跑通了：`device runner-status`、`relay`、`serve` + `/agent/status`、`stop`（Win32 进程身份校验 + TerminateProcess）。

**未验证**（下一步的重点）：
1. 真实的 Apple Mobile Device Service 是否响应 `ReadPairRecord`（影响 `device ddi`；回退到读 `%ProgramData%\Apple\Lockdown` 的文件）。
2. Sideloadly 签过的 IPA 能否被 `ios runtest` 拉起（XCTest 相关框架是否都签上了）。
3. go-ios userspace 隧道在 Windows 上对 iOS 17+ 是否可用。
4. 真机上点击、读屏、MJPEG 画面是否正常。
5. `iphone-use.ps1` 在 PowerShell 5.1 上实际运行（只做过代码审阅，没真正执行过）。

## 5. 实机调试步骤

### 准备
1. 安装微软商店的 **Apple Devices**（或 iTunes）。插上 iPhone、解锁、点"信任"。
2. 下载 go-ios Windows 版 `ios.exe`：<https://github.com/danielpaulus/go-ios/releases>
3. 从 [Actions → Windows](https://github.com/Souitou-iop/iphone-use/actions/workflows/windows.yml) 最新一次成功运行的 Artifacts 下载：
   - `iphone-use-windows-x64`（exe + ps1 + 文档）
   - `iPhoneUse-Runner-unsigned-ipa`
4. 用 Sideloadly 侧载 IPA。手机上：设置 → 通用 → VPN 与设备管理 → 信任；打开开发者模式（手机会重启）。
   **点 runner 图标会闪退，这是正常的**，它只能由 XCTest 启动。

### 分步排查（建议先手动，别直接跑一键脚本）

在解压目录打开 PowerShell，按顺序执行，每步确认正常再往下：

```powershell
# ① usbmuxd 和手机：应输出 {"deviceList":["<udid>"]}
.\ios.exe list
# ② 本项目自己的 usbmux 实现：应输出 ok:true、name、product_version
.\iphone-use.exe device info --udid <udid>
# ③ 开发者磁盘镜像：应返回 mounted:true（失败也不影响后续，go-ios 会自己挂）
.\iphone-use.exe device ddi --udid <udid>
# ④ iOS 17+ 才需要；另开一个窗口，保持运行
.\ios.exe tunnel start --userspace
# ⑤ 挂载开发者磁盘镜像
.\ios.exe image auto
# ⑥ 找 runner 的 bundle id（结尾 .xctrunner）
.\ios.exe apps --list
# ⑦ 启动 runner；另开一个窗口，保持运行。日志里出现 ServerURLHere-> 即成功
.\ios.exe runtest --bundle-id=<id> --test-runner-bundle-id=<id> --xctest-config=iPhoneUse.xctest --test-to-run=RunnerTests/testServe
# ⑧ 直接经 usbmuxd 问 runner：应返回 session_id、state
.\iphone-use.exe device runner-status --udid <udid>
# ⑨ 端口转发（各开一个窗口）
.\iphone-use.exe relay --udid <udid> --listen 127.0.0.1:8100 --device-port 8100
.\iphone-use.exe relay --udid <udid> --listen 127.0.0.1:9100 --device-port 9100
# ⑩ 守护进程
$env:PHONE_REMOTE_UDID = "<udid>"; .\iphone-use.exe serve
# 然后浏览器打开 http://127.0.0.1:44321/phone，或者：
curl.exe http://127.0.0.1:44321/agent/status
```

都通了之后再试一键脚本：`powershell -ExecutionPolicy Bypass -File .\iphone-use.ps1`（日志在 `%USERPROFILE%\.iphone-use\logs\`）。

### 各步失败时去哪看

| 现象 | 可能原因 | 相关代码 / 处理 |
|---|---|---|
| ① 无设备 | Apple Devices 没装好、没点信任 | — |
| ② 报 `connect usbmuxd` | 27015 端口不通 | `usbmux.rs` 的 `open_mux` / `mux_address`；可设 `USBMUXD_SOCKET_ADDRESS` |
| ③ 报 `no pairing record` | AMDS 不支持 `ReadPairRecord`，文件回退也没读到 | `usbmux.rs` 的 `read_pair_record`；检查 `%ProgramData%\Apple\Lockdown\` 下文件名大小写、有没有横杠 |
| ③ 报 TLS 错误 | Windows 上的配对记录格式不同（例如二进制 plist） | `lockdown.rs` 的 `PairRecord::parse`；这里只认 XML plist |
| ⑦ 报签名或 dylib 错误 | Sideloadly 没签好 `Frameworks` / `PlugIns` | 换成 `ios sign app --path <ipa> --p12file … --profile … --install` 自签 |
| ⑦ 报连不上 testmanagerd | iOS 17+ 隧道没起来 | 看 ④ 的输出；试试以管理员身份运行、去掉 `--userspace` |
| ⑦ 正常，但 ⑧ 失败 | runner 端口没监听 | 看 ⑦ 的日志有没有 `ServerURLHere` |
| ⑩ 状态里 `drivable:false` | 健康探测失败 | `http.rs` 的 `/agent/status`、`wda.rs` 的健康探测；runner 的日志 |
| `iphone-use stop` 失败 | 进程身份不匹配 | `main.rs` 的 `read_process_identity` / `terminate`（Windows 版） |

## 6. 开发与编译

- Windows 本地编译：装 Rust（rustup）+ Visual Studio Build Tools（C++ 工作负载），然后执行
  `cargo build --release --bin iphone-use --bin iphone-use-mcp`。
- 在 Linux 上交叉检查（不需要 Windows 机器）：
  ```bash
  sudo apt-get install gcc-mingw-w64-x86-64
  rustup target add x86_64-pc-windows-gnu
  cargo check --workspace --target x86_64-pc-windows-gnu
  ```
  **注意**：GNU 版 libc 比 MSVC 版多一些符号（踩过 `libc::STDERR_FILENO` 的坑），能过 GNU 不代表能过 MSVC，最终以 Windows CI 为准。尽量用标准库，少直接用 `libc::`。
- Wine 冒烟测试：`apt-get install wine`，然后 `wine target/x86_64-pc-windows-gnu/release/iphone-use.exe --version`。Wine 里不设置 `HOME`，正好能测 `USERPROFILE` 回退。
- 仓库约定（见 `AGENTS.md`）：多个会话共用工作区，提交前先 `git status`，**按文件** `git add`，不要用 `git add -A`。
- CI 里 Python 读子进程输出要显式写 `encoding="utf-8"`（Windows 默认是 cp1252）。

## 7. 后续待办（按优先级）

1. **实机跑通第 5 节**，把发现的问题修掉。
2. **runner 掉线自动拉起**：目前 Windows 上 runner 挂了只能重跑脚本。可以在 ps1 里循环重启 `ios runtest`，或在守护进程里加一个 Windows 版的托管实现（现在托管逻辑依赖 `setup-wda.sh` + `launchctl`，见 `http.rs` 里的 `managed_wda`）。
3. **开机自启 / 后台服务**：用计划任务或 NSSM 替代 LaunchAgent。
4. **`/agent/apps`**：现在依赖 `devicectl`，Windows 上返回不可用；可以改用 go-ios `ios apps` 或 installation_proxy。
5. **`stop` 优雅退出**：现在是 `TerminateProcess`；可以改成守护进程监听一个停止信号（命名事件或本地 HTTP 端点）。
6. **H.264 画面**：Windows 上可以用 Media Foundation 编码，目前只有 MJPEG。
7. **签名门槛**：研究 go-ios `ios sign app` 自签流程，写进文档，减少对 Sideloadly 的依赖。
8. 是否向上游（leeguooooo/iphone-use）提 PR：建议实机跑通后再提。改动都用 `cfg(windows)` 隔开，对 Mac 端无影响，上游接受的可能性较高。

## 8. 关键环境变量

| 变量 | 作用 |
|---|---|
| `PHONE_REMOTE_UDID` | 目标手机 |
| `PHONE_REMOTE_WDA_URL` / `PHONE_REMOTE_WDA_MJPEG_URL` | runner 地址，默认 `127.0.0.1:8100` / `:9100`；可以直接指向手机的 Wi-Fi IP，不经过 relay |
| `PHONE_REMOTE_WDA_MANAGED` | Windows 默认 `false` |
| `PHONE_REMOTE_HOST` / `PHONE_REMOTE_PORT` | 监听地址，默认 `127.0.0.1:44321`；设成 `0.0.0.0` 时必须同时设密码 |
| `PHONE_REMOTE_AGENT_TOKEN` / `PHONE_REMOTE_PASSWORD` | 鉴权；ps1 会生成 token，存到 `%USERPROFILE%\.iphone-use\agent-token` |
| `PHONE_REMOTE_URL` / `PHONE_REMOTE_TOKEN` | MCP 桥连接守护进程用（Windows 上没有 LaunchAgent plist 可读，必须设） |
| `USBMUXD_SOCKET_ADDRESS` | 覆盖 usbmuxd 地址（`host:port` 或 Unix 路径） |
