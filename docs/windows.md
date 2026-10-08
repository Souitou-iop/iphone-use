# iphone-use on Windows (experimental)

[简体中文](windows.zh-CN.md)

The daemon (`iphone-use.exe`) and the MCP bridge (`iphone-use-mcp.exe`) build and run
natively on Windows 10/11 x64. The agent API, the MCP tools, flows and the web page
(`/phone`, MJPEG live view) work the same as on a Mac. What a Mac does with Xcode and a
LaunchAgent is done by hand or by `iphone-use.ps1` here.

| | macOS | Windows |
|---|---|---|
| usbmuxd | macOS's own | Apple Mobile Device Service (`127.0.0.1:27015`) |
| Build and sign the runner | `setup-wda.sh` + Xcode | download the unsigned IPA, sign it yourself |
| Start the runner (XCTest) | `xcodebuild test-without-building` | [go-ios](https://github.com/danielpaulus/go-ios) `ios runtest` |
| Keep it running | LaunchAgent supervisor | `iphone-use.ps1` (foreground; restart it if the runner dies) |
| Live view | H.264 (VideoToolbox) or MJPEG | MJPEG |
| `setup`, `doctor`, `upgrade`, idle release, installed-app list | yes | not yet |

## What you need

- Windows 10/11 x64.
- **Apple Devices** from the Microsoft Store (or iTunes). It installs Apple Mobile Device
  Service, the Windows usbmuxd. Plug the iPhone in, unlock it and tap **Trust**.
- **Developer Mode** on the iPhone (Settings → Privacy & Security → Developer Mode). On
  iOS 16+ the switch only appears after a developer app has been installed once, so turn
  it on after installing the runner.
- **go-ios** for Windows (`ios.exe`) from its [releases](https://github.com/danielpaulus/go-ios/releases).
- A way to sign an IPA with your Apple ID: [Sideloadly](https://sideloadly.io/) (free Apple
  ID works), or `ios sign app --path <ipa> --p12file … --profile … --install` (go-ios 1.3.2) with your own `.p12` certificate and provisioning profile.
- `iphone-use-windows-x64.zip` and `iPhoneUse-Runner-unsigned.ipa` from the release (or
  the *Windows* workflow's artifacts).

## 1. Install the runner on the phone

1. Open `iPhoneUse-Runner-unsigned.ipa` in Sideloadly, sign in with your Apple ID and
   install. If the bundle id is taken, let Sideloadly change it; keep `.xctrunner` at the
   end. With a free Apple ID the signature lasts 7 days; install again after that.
2. On the phone: Settings → General → VPN & Device Management → trust your Apple ID.
3. Turn Developer Mode on if it was not already (the phone restarts).

Tapping the iPhoneUse-Runner icon opens and immediately closes it. **That is expected**: it is
an XCTest runner (like WebDriverAgent's) and only runs when the test framework starts it
(`ios runtest`, below). Whether it is installed correctly shows in step 2, when the script
waits for the runner to come up.

## 2. Start everything

Unzip `iphone-use-windows-x64.zip`, put `ios.exe` in the same folder, then in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File .\iphone-use.ps1
```

The script:

1. checks Apple Mobile Device Service and finds the phone;
2. on iOS 17+, starts the go-ios tunnel agent (`ios tunnel start --userspace`);
3. mounts the Developer Disk Image (`ios image auto`);
4. starts the runner's XCTest (`ios runtest … --test-to-run=RunnerTests/testServe`);
5. relays the runner's ports with `iphone-use relay` (`127.0.0.1:8100` control,
   `127.0.0.1:9100` video);
6. runs `iphone-use serve` with a generated agent token (`%USERPROFILE%\.iphone-use\agent-token`).

Then open `http://127.0.0.1:44321/phone`. Ctrl+C stops everything. Logs are in
`%USERPROFILE%\.iphone-use\logs`. Options: `-Udid`, `-BundleId`, `-ListenHost 0.0.0.0
-Password …` (LAN access), `-Port`, `-NoTunnel`.

## 3. Connect an agent

```json
{
  "mcpServers": {
    "iphone-use": {
      "command": "C:\\path\\to\\iphone-use-mcp.exe",
      "env": {
        "PHONE_REMOTE_URL": "http://127.0.0.1:44321",
        "PHONE_REMOTE_TOKEN": "<contents of %USERPROFILE%\\.iphone-use\\agent-token>"
      }
    }
  }
}
```

Or call the HTTP API with `Authorization: Bearer <token>` as in the [guide](guide.md).

## Doing it by hand

Any way of starting the runner works; the daemon only needs its two ports:

```powershell
ios tunnel start --userspace                  # iOS 17+, keep running
ios image auto
ios runtest --bundle-id=<id>.xctrunner --test-runner-bundle-id=<id>.xctrunner `
    --xctest-config=iPhoneUse.xctest --test-to-run=RunnerTests/testServe
iphone-use relay --udid <udid> --listen 127.0.0.1:8100 --device-port 8100
iphone-use relay --udid <udid> --listen 127.0.0.1:9100 --device-port 9100
$env:PHONE_REMOTE_UDID = "<udid>"; iphone-use serve
```

The runner also listens on the phone's Wi-Fi address, so `PHONE_REMOTE_WDA_URL=http://<phone-ip>:8100`
and `PHONE_REMOTE_WDA_MJPEG_URL=http://<phone-ip>:9100` work without relays (those ports
have no authentication; use a trusted network). `USBMUXD_SOCKET_ADDRESS=host:port`
points `relay` and `device` at another usbmuxd.

## Known limits

- The daemon cannot restart the runner itself on Windows (`PHONE_REMOTE_WDA_MANAGED` is
  off). If the runner stops (phone restarted, signature expired), run the script again.
- No H.264 live view; the web page uses MJPEG (more bandwidth, fine on a LAN).
- `/agent/apps` (installed apps) needs `devicectl` and reports unavailable.
- `iphone-use stop` terminates the daemon instead of a graceful SIGTERM.
- Not tested by the maintainers on real hardware yet; please report issues.
