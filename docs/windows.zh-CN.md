# 在 Windows 上使用 iphone-use（实验性）

[English](windows.md)

守护进程（`iphone-use.exe`）和 MCP 桥（`iphone-use-mcp.exe`）可以在 Windows 10/11 x64 上原生编译、运行。
Agent API、MCP 工具、流程（flows）和网页控制台（`/phone`，MJPEG 实时画面）与 Mac 上一致。
Mac 上由 Xcode 和 LaunchAgent 完成的部分，在 Windows 上改为手动或由 `iphone-use.ps1` 完成。

| | macOS | Windows |
|---|---|---|
| usbmuxd | 系统自带 | Apple Mobile Device Service（`127.0.0.1:27015`） |
| 编译、签名 runner | `setup-wda.sh` + Xcode | 下载未签名 IPA，自行签名 |
| 启动 runner（XCTest） | `xcodebuild test-without-building` | [go-ios](https://github.com/danielpaulus/go-ios) `ios runtest` |
| 保活 | LaunchAgent 守护 | `iphone-use.ps1`（前台运行；runner 挂了需重跑） |
| 实时画面 | H.264（VideoToolbox）或 MJPEG | MJPEG |
| `setup`、`doctor`、`upgrade`、空闲释放、已装应用列表 | 有 | 暂无 |

## 准备

- Windows 10/11 x64。
- 微软商店的 **Apple Devices**（或 iTunes），它带有 Windows 版 usbmuxd（Apple Mobile Device Service）。
  插上 iPhone、解锁并点 **信任**。
- iPhone 打开 **开发者模式**（设置 → 隐私与安全性 → 开发者模式）。iOS 16+ 要先装过一个开发者 App 才会出现这个开关，
  所以装好 runner 之后再打开。
- Windows 版 **go-ios**（`ios.exe`），见其 [releases](https://github.com/danielpaulus/go-ios/releases)。
- 用 Apple ID 给 IPA 签名的工具：[Sideloadly](https://sideloadly.io/)（免费 Apple ID 即可），
  或用自己的 `.p12` 证书和描述文件执行 `ios ui install`。
- Release（或 *Windows* 工作流产物）里的 `iphone-use-windows-x64.zip` 和 `iPhoneUse-Runner-unsigned.ipa`。

## 1. 在手机上安装 runner

1. 用 Sideloadly 打开 `iPhoneUse-Runner-unsigned.ipa`，登录 Apple ID 后安装。若 bundle id 冲突，让 Sideloadly
   改一个，结尾保留 `.xctrunner`。免费 Apple ID 的签名 7 天有效，过期后重装。
2. 手机上：设置 → 通用 → VPN 与设备管理 → 信任你的 Apple ID。
3. 如果还没开开发者模式，现在打开（手机会重启）。

在主屏幕上点 iPhoneUse-Runner 图标，它会一闪就退出，**这是正常的**：它是 XCTest 运行器（和 WebDriverAgent 一样），
只能由测试框架（下面的 `ios runtest`）启动，不能当普通 App 打开。是否装好，以第 2 步脚本能不能等到 runner 上线为准。

## 2. 启动

解压 `iphone-use-windows-x64.zip`，把 `ios.exe` 放进同一目录，在 PowerShell 中运行：

```powershell
powershell -ExecutionPolicy Bypass -File .\iphone-use.ps1
```

脚本会依次：

1. 检查 Apple Mobile Device Service 并找到手机；
2. iOS 17+ 时启动 go-ios 隧道（`ios tunnel start --userspace`）；
3. 挂载开发者磁盘镜像（`ios image auto`）；
4. 启动 runner 的 XCTest（`ios runtest … --test-to-run=RunnerTests/testServe`）；
5. 用 `iphone-use relay` 转发 runner 端口（`127.0.0.1:8100` 控制、`127.0.0.1:9100` 画面）；
6. 用生成的 agent token（`%USERPROFILE%\.iphone-use\agent-token`）运行 `iphone-use serve`。

然后打开 `http://127.0.0.1:44321/phone`。Ctrl+C 全部停止。日志在 `%USERPROFILE%\.iphone-use\logs`。
可选参数：`-Udid`、`-BundleId`、`-ListenHost 0.0.0.0 -Password …`（局域网访问）、`-Port`、`-NoTunnel`。

## 3. 接入 Agent

```json
{
  "mcpServers": {
    "iphone-use": {
      "command": "C:\\path\\to\\iphone-use-mcp.exe",
      "env": {
        "PHONE_REMOTE_URL": "http://127.0.0.1:44321",
        "PHONE_REMOTE_TOKEN": "<%USERPROFILE%\\.iphone-use\\agent-token 的内容>"
      }
    }
  }
}
```

也可以按 [指南](guide.zh-CN.md) 直接用 `Authorization: Bearer <token>` 调 HTTP API。

## 手动启动

runner 用什么方式启动都可以，守护进程只需要它的两个端口：

```powershell
ios tunnel start --userspace                  # iOS 17+，保持运行
ios image auto
ios runtest --bundle-id=<id>.xctrunner --test-runner-bundle-id=<id>.xctrunner `
    --xctest-config=iPhoneUse.xctest --test-to-run=RunnerTests/testServe
iphone-use relay --udid <udid> --listen 127.0.0.1:8100 --device-port 8100
iphone-use relay --udid <udid> --listen 127.0.0.1:9100 --device-port 9100
$env:PHONE_REMOTE_UDID = "<udid>"; iphone-use serve
```

runner 也监听手机的 Wi-Fi 地址，所以可以不用 relay，直接设
`PHONE_REMOTE_WDA_URL=http://<手机IP>:8100`、`PHONE_REMOTE_WDA_MJPEG_URL=http://<手机IP>:9100`
（这两个端口没有鉴权，只在可信网络使用）。`USBMUXD_SOCKET_ADDRESS=host:port` 可让 `relay`、`device` 连别的 usbmuxd。

## 已知限制

- Windows 上守护进程不能自己重启 runner（`PHONE_REMOTE_WDA_MANAGED` 关闭）。runner 停了（手机重启、签名过期）就重跑脚本。
- 没有 H.264 画面，网页用 MJPEG（带宽高一些，局域网没问题）。
- `/agent/apps`（已装应用）依赖 `devicectl`，会返回不可用。
- `iphone-use stop` 是直接结束进程，而不是 SIGTERM 优雅退出。
- 维护者尚未在真机上测试，欢迎反馈问题。
