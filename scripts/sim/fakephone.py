"""A simulated iPhone for exercising the Windows chain on Linux.

  usbmuxd  TCP 127.0.0.1:27015  ListDevices / Connect / ReadPairRecord
  lockdown device port 62078    GetValue (plain), StartSession refused
  runner   device ports 8100/9100, only while `fakeios runtest` runs (it
           serves them on 127.0.0.1:18100 / 19100)

Run: python3 fakephone.py [--no-device]
"""
import json, os, plistlib, socket, struct, sys, threading

UDID = "00008110-0002346211A0401E"
VALUES = {"DeviceName": "Sim iPhone", "ProductVersion": "18.6", "ProductType": "iPhone14,5",
          "BuildVersion": "22G86"}
DEVICE_PORT_MAP = {8100: 18100, 9100: 19100}
ATTACHED = "--no-device" not in sys.argv


def pipe(a, b):
    try:
        while (d := a.recv(65536)):
            b.sendall(d)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def mux_reply(conn, tag, obj):
    data = plistlib.dumps(obj, fmt=plistlib.FMT_XML)
    conn.sendall(struct.pack("<IIII", 16 + len(data), 1, 8, tag) + data)


def lockdown(conn):
    """Plain lockdownd: 4-byte big-endian length + XML plist."""
    try:
        while True:
            hdr = conn.recv(4, socket.MSG_WAITALL)
            if len(hdr) < 4:
                return
            msg = plistlib.loads(conn.recv(struct.unpack(">I", hdr)[0], socket.MSG_WAITALL))
            req = msg.get("Request")
            if req == "GetValue":
                out = {"Request": req, "Key": msg.get("Key"), "Value": VALUES.get(msg.get("Key"), "")}
            elif req == "QueryType":
                out = {"Request": req, "Type": "com.apple.mobile.lockdown"}
            else:
                out = {"Request": req, "Error": "InvalidHostID"}
            data = plistlib.dumps(out, fmt=plistlib.FMT_XML)
            conn.sendall(struct.pack(">I", len(data)) + data)
    except OSError:
        pass
    finally:
        conn.close()


def mux(conn):
    try:
        hdr = conn.recv(16, socket.MSG_WAITALL)
        if len(hdr) < 16:
            return
        length, _, _, tag = struct.unpack("<IIII", hdr)
        msg = plistlib.loads(conn.recv(length - 16, socket.MSG_WAITALL))
    except Exception:
        conn.close()
        return
    kind = msg.get("MessageType")
    if kind == "ListDevices":
        devices = [{"DeviceID": 7, "MessageType": "Attached",
                    "Properties": {"SerialNumber": UDID, "ConnectionType": "USB", "DeviceID": 7}}] if ATTACHED else []
        mux_reply(conn, tag, {"DeviceList": devices})
        conn.close()
    elif kind == "ReadBUID":
        mux_reply(conn, tag, {"BUID": "SIM-BUID"})
        conn.close()
    elif kind == "ReadPairRecord":
        # Like a host that never paired: the GUI must report it, not crash.
        mux_reply(conn, tag, {"MessageType": "Result", "Number": 2})
        conn.close()
    elif kind == "Connect":
        port = socket.ntohs(msg["PortNumber"])
        if port == 62078:
            mux_reply(conn, tag, {"MessageType": "Result", "Number": 0})
            lockdown(conn)
            return
        local = DEVICE_PORT_MAP.get(port)
        try:
            up = socket.create_connection(("127.0.0.1", local), timeout=1) if local else None
        except OSError:
            up = None
        if up is None:
            mux_reply(conn, tag, {"MessageType": "Result", "Number": 3})
            conn.close()
            return
        up.settimeout(None)
        mux_reply(conn, tag, {"MessageType": "Result", "Number": 0})
        threading.Thread(target=pipe, args=(up, conn), daemon=True).start()
        pipe(conn, up)
    else:
        mux_reply(conn, tag, {"MessageType": "Result", "Number": 1})
        conn.close()


srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 27015))
srv.listen(64)
print(f"fake usbmuxd on 127.0.0.1:27015 (device attached: {ATTACHED})", flush=True)
while True:
    c, _ = srv.accept()
    threading.Thread(target=mux, args=(c,), daemon=True).start()
