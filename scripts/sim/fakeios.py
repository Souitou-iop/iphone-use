#!/usr/bin/env python3
"""Fake go-ios 1.3.2 (`ios`): `list` and `version` go to the real binary when
REAL_IOS is set (it talks to the fake usbmuxd); the rest is simulated with
go-ios's own output shapes. Logs go to stderr like go-ios's JSON logs."""
import json, os, socket, subprocess, sys

SIM = os.path.dirname(os.path.abspath(__file__))
RUNNER_ID = os.environ.get("SIM_RUNNER_ID", "com.leeguoo.iphone-use.runner.xctrunner")
args = sys.argv[1:]
cmd = [a for a in args if not a.startswith("-")]
real = os.environ.get("REAL_IOS")


def log(msg, level="INFO"):
    print(json.dumps({"level": level, "msg": msg}), file=sys.stderr, flush=True)


if cmd[:1] in (["list"], ["version"]) and real:
    sys.exit(subprocess.call([real] + args))
if cmd[:1] == ["version"]:
    print(json.dumps({"version": "sim"}))
    sys.exit(0)
if cmd[:1] == ["list"]:
    # Ask the fake usbmuxd, like go-ios would.
    import plistlib, struct
    body = plistlib.dumps({"MessageType": "ListDevices"}, fmt=plistlib.FMT_XML)
    s = socket.create_connection(("127.0.0.1", 27015))
    s.sendall(struct.pack("<IIII", 16 + len(body), 1, 8, 1) + body)
    length = struct.unpack("<IIII", s.recv(16, socket.MSG_WAITALL))[0]
    reply = plistlib.loads(s.recv(length - 16, socket.MSG_WAITALL))
    print(json.dumps({"deviceList": [d["Properties"]["SerialNumber"] for d in reply["DeviceList"]]}))
    sys.exit(0)
if cmd[:1] == ["apps"]:
    for line in ["com.apple.Preferences Settings 18.6", f"{RUNNER_ID} iPhoneUse-Runner 0.2.0"]:
        print(line)
    sys.exit(0)
if cmd[:2] == ["image", "auto"]:
    log("success mounting image")
    sys.exit(0)
if cmd[:2] == ["tunnel", "start"]:
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", 28100))
    s.listen(8)
    log("tunnel agent listening on 28100")
    while True:
        c, _ = s.accept()
        c.sendall(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[]")
        c.close()
if cmd[:1] == ["runtest"]:
    want = [a.split("=", 1)[1] for a in args if a.startswith("--bundle-id=")]
    if want and want[0] != RUNNER_ID:
        log(f"app {want[0]} not installed", "ERROR")
        sys.exit(1)
    sys.path.insert(0, SIM)
    import fakerunner
    log("Starting test runner")
    fakerunner.serve()
log(f"fake ios: unsupported {args}", "ERROR")
sys.exit(1)
