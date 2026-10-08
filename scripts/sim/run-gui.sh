#!/bin/bash
# Run `iphone-use gui` against the simulated phone (no iPhone, no Windows).
#   scripts/sim/run-gui.sh [path/to/iphone-use]   then open http://127.0.0.1:44390/
# Ctrl+C stops the page, everything it started and the fake usbmuxd.
set -euo pipefail
SIM="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$SIM/../../target/debug/iphone-use}"
python3 -I "$SIM/fakephone.py" &
PHONE=$!
trap 'kill $PHONE 2>/dev/null' EXIT
sleep 0.5
USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015 IPHONE_USE_FLOWS_NO_AUTO_UPDATE=1 \
    "$BIN" gui --no-open --ios "$SIM/fakeios.py"
