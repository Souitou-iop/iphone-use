# Simulated iPhone for the Windows chain

A phone you can run on any machine with Python 3 and Pillow, for working on
the Windows path (`iphone-use gui`, `scripts/windows/iphone-use.ps1`,
`relay`, `serve`, MCP) without an iPhone or a Windows box.

| File | Plays |
|---|---|
| `fakephone.py` | usbmuxd on `127.0.0.1:27015` (where Apple Mobile Device Service listens on Windows): `ListDevices`, `Connect`, `ReadPairRecord` (answers "not paired"); lockdownd `GetValue` on device port 62078. `--no-device` lists no phone. |
| `fakerunner.py` | the device runner: the WDA routes `WdaClient` uses, a Home screen with a Settings icon and a Settings screen, screenshots, MJPEG. Serves device ports 8100/9100 on `127.0.0.1:18100/19100`. |
| `fakeios.py` | go-ios 1.3.2's `list`, `version`, `apps --list`, `image auto`, `tunnel start` (agent on 28100) and `runtest` (runs `fakerunner.py`; killing it is "the runner died"). Set `REAL_IOS=<go-ios binary>` to send `list`/`version` to the real go-ios. |

```bash
cargo build --bin iphone-use --bin iphone-use-mcp
scripts/sim/run-gui.sh            # then open http://127.0.0.1:44390/ and press 一键启动
```

Everything `iphone-use` does against it goes through the same code as on
Windows: `USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015` makes the Unix build use
the TCP usbmuxd path, and video falls back to MJPEG off macOS.

The Windows build itself can be run under Wine against the same fake phone
(`wine iphone-use.exe gui`; the real Windows go-ios `ios.exe list` finds the
fake phone on 27015 too).
