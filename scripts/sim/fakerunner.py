"""WDA-compatible fake runner: the routes WdaClient uses, a two-screen phone
(Home with a Settings icon, and Settings) and an MJPEG stream."""
import base64, io, json, re, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image, ImageDraw

W, H = 390, 844
STATE = {"screen": "home", "session": None}
LOCK = threading.Lock()
ICONS = [("Settings", 30, 80), ("Camera", 120, 80), ("Photos", 210, 80), ("Safari", 300, 80)]


def tree():
    if STATE["screen"] == "home":
        children = [{"type": "XCUIElementTypeIcon", "label": n, "name": n, "isEnabled": True,
                     "rect": {"x": x, "y": y, "width": 60, "height": 60}, "children": []} for n, x, y in ICONS]
        return {"type": "XCUIElementTypeApplication", "label": " ", "name": "SpringBoard", "isEnabled": True,
                "rect": {"x": 0, "y": 0, "width": W, "height": H}, "children": children}
    rows = ["General", "Privacy & Security", "Battery"]
    children = [{"type": "XCUIElementTypeNavigationBar", "label": "Settings", "name": "Settings", "isEnabled": True,
                 "rect": {"x": 0, "y": 47, "width": W, "height": 96}, "children": []}]
    for i, r in enumerate(rows):
        children.append({"type": "XCUIElementTypeCell", "label": r, "name": r, "isEnabled": True,
                         "rect": {"x": 16, "y": 160 + i * 52, "width": W - 32, "height": 52}, "children": []})
    return {"type": "XCUIElementTypeApplication", "label": "Settings", "name": "Settings", "isEnabled": True,
            "rect": {"x": 0, "y": 0, "width": W, "height": H}, "children": children}


def render(fmt):
    img = Image.new("RGB", (W, H), (28, 60, 110) if STATE["screen"] == "home" else (242, 242, 247))
    d = ImageDraw.Draw(img)
    d.text((16, 14), time.strftime("%H:%M:%S"), fill=(255, 255, 255) if STATE["screen"] == "home" else (0, 0, 0))
    if STATE["screen"] == "home":
        colors = [(142, 142, 147), (60, 60, 60), (255, 149, 0), (0, 122, 255)]
        for (n, x, y), c in zip(ICONS, colors):
            d.rounded_rectangle((x, y, x + 60, y + 60), 14, fill=c)
            d.text((x + 4, y + 66), n, fill=(255, 255, 255))
        d.text((110, 700), "SIMULATED iPHONE", fill=(255, 255, 255))
    else:
        d.text((16, 100), "Settings", fill=(0, 0, 0))
        for i, r in enumerate(["General", "Privacy & Security", "Battery"]):
            d.rectangle((16, 160 + i * 52, W - 16, 210 + i * 52), fill=(255, 255, 255))
            d.text((32, 178 + i * 52), r, fill=(0, 0, 0))
    buf = io.BytesIO()
    img.save(buf, fmt)
    return buf.getvalue()


def tap(x, y):
    with LOCK:
        if STATE["screen"] == "home":
            for n, ix, iy in ICONS:
                if ix <= x <= ix + 60 and iy <= y <= iy + 60 and n == "Settings":
                    STATE["screen"] = "settings"


class WDA(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send(self, value, status=200):
        body = json.dumps({"value": value, "sessionId": STATE["session"]}).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            return json.loads(raw or b"{}")
        except Exception:
            return {}

    def route(self, method):
        path = self.path.split("?")[0]
        path = re.sub(r"^/session/[^/]+", "/S", path)
        data = self.body() if method == "POST" else {}
        if path == "/status":
            return self.send({"ready": True, "state": "success", "message": "fake runner"})
        if path in ("/wda/locked", "/S/wda/locked"):
            return self.send(False)
        if method == "POST" and self.path == "/session":
            STATE["session"] = "SIM-SESSION"
            return self.send({"sessionId": "SIM-SESSION", "capabilities": {}})
        if path == "/S/appium/settings":
            return self.send({})
        if path == "/S/wda/apps/list":
            bundle = "com.apple.springboard" if STATE["screen"] == "home" else "com.apple.Preferences"
            return self.send([{"pid": 1, "bundleId": bundle}])
        if path == "/source":
            return self.send(tree())
        if path == "/screenshot":
            return self.send(base64.b64encode(render("PNG")).decode())
        if path == "/S/window/size":
            return self.send({"width": W, "height": H})
        if path == "/S/actions":
            for act in data.get("actions", []):
                xs = [a for a in act.get("actions", []) if a.get("type") == "pointerMove"]
                if xs:
                    tap(xs[0].get("x", 0), xs[0].get("y", 0))
            return self.send(None)
        if path == "/S/wda/pressButton":
            if data.get("name") == "home":
                STATE["screen"] = "home"
            return self.send(None)
        if path in ("/S/elements", "/S/element") and method == "POST":
            value = str(data.get("value", ""))
            found = [c for c in tree()["children"] if c["label"] and c["label"] in value]
            ids = [{"ELEMENT": c["label"], "element-6066-11e4-a52e-4f735466cecf": c["label"]} for c in found]
            if path == "/S/element":
                return self.send(ids[0]) if ids else self.send({"error": "no such element", "message": value}, 404)
            return self.send(ids)
        m = re.match(r"^/S/element/([^/]+)/(rect|click)$", path)
        if m:
            label = m.group(1).replace("%20", " ")
            node = next((c for c in tree()["children"] if c["label"] == label), None)
            if node is None:
                return self.send({"error": "stale element reference", "message": label}, 404)
            r = node["rect"]
            if m.group(2) == "click":
                tap(r["x"] + r["width"] / 2, r["y"] + r["height"] / 2)
                return self.send(None)
            return self.send(r)
        if path.startswith("/S/alert"):
            return self.send({"error": "no such alert", "message": "no alert"}, 404)
        if path == "/S/wda/apps/launch":
            if data.get("bundleId") == "com.apple.Preferences":
                STATE["screen"] = "settings"
            return self.send(None)
        print(f"UNIMPLEMENTED {method} {self.path} {json.dumps(data)[:200]}", flush=True)
        return self.send({"error": "unknown command", "message": f"{method} {self.path}"}, 404)

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def do_DELETE(self):
        self.send(None)

    def log_message(self, *a):
        pass


class MJPEG(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=--BoundaryString")
        self.end_headers()
        try:
            while True:
                frame = render("JPEG")
                self.wfile.write(b"--BoundaryString\r\nContent-type: image/jpg\r\nContent-Length: %d\r\n\r\n" % len(frame))
                self.wfile.write(frame + b"\r\n\r\n")
                time.sleep(0.2)
        except OSError:
            pass

    def log_message(self, *a):
        pass


def serve():
    threading.Thread(target=ThreadingHTTPServer(("127.0.0.1", 19100), MJPEG).serve_forever, daemon=True).start()
    srv = ThreadingHTTPServer(("127.0.0.1", 18100), WDA)
    print("ServerURLHere->http://192.168.1.50:8100<-ServerURLHere", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    serve()
