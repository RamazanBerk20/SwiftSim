#!/bin/bash
# Regenerates docs/transient.png and docs/ac.png in a private, headless KWin
# (kwin_wayland --virtual on its own D-Bus, Wayland socket and Xwayland). Nothing
# is shown on, typed into or captured from your desktop.
# Needs: kwin_wayland, Xwayland, dbus-run-session, spectacle, xdotool, python3,
# and a release build (cargo build --release).
#   scripts/screenshots.sh [WORKDIR]   (WORKDIR: short path, default ./target/shots)
set -euo pipefail
REPO=$(cd "$(dirname "$0")/.." && pwd)
S=$REPO/target/release/swiftsim
ROOT=$(mkdir -p "${1:-$REPO/target/shots}" && cd "${1:-$REPO/target/shots}" && pwd)
rm -rf "$ROOT/n" && mkdir -p "$ROOT/n/home/user/.config" && install -d -m 700 "$ROOT/n/r"
N=$ROOT/n
ISO=(HOME="$N/home/user" XDG_RUNTIME_DIR="$N/r" XDG_CONFIG_HOME="$N/home/user/.config"
  XDG_DATA_HOME="$N/home/user/.local/share" XDG_CACHE_HOME="$N/home/user/.cache"
  XDG_STATE_HOME="$N/home/user/.local/state" LANG=C.UTF-8 PATH="$PATH")

# Every window maximized without borders (screen == app); xdotool input on the
# nest's Xwayland without KWin's permission prompt.
printf '[Windows]\nBorderlessMaximizedWindows=true\n\n[Xwayland]\nXwaylandEisNoPrompt=true\n' >"$N/home/user/.config/kwinrc"
printf '[General]\ncount=1\nrules=1\n\n[1]\nDescription=max\nmaximizehoriz=true\nmaximizehorizrule=3\nmaximizevert=true\nmaximizevertrule=3\ntypes=1\n' \
  >"$N/home/user/.config/kwinrulesrc"
printf '#!/bin/sh\necho $$ > "%s/child.pid"\nenv > "%s/kwin-env"\nexec sleep 100000\n' "$N" "$N" >"$N/child.sh" && chmod 700 "$N/child.sh"
setsid env -i "${ISO[@]}" KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1 \
  dbus-run-session -- kwin_wayland --virtual --xwayland --socket wl --width 1280 --height 800 "$N/child.sh" \
  >"$N/kwin.log" 2>&1 &
KWIN=$!
APP=
cleanup() {
  [[ -z $APP ]] || kill "$APP" 2>/dev/null || true
  kill -TERM -- "-$KWIN" 2>/dev/null || true; sleep 1; kill -KILL -- "-$KWIN" 2>/dev/null || true
  # KWin starts its child in a session of its own.
  [[ ! -s $N/child.pid ]] || kill "$(cat "$N/child.pid")" 2>/dev/null || true
}
trap cleanup EXIT
for _ in $(seq 100); do [[ -s $N/kwin-env ]] && break; sleep 0.2; done
sleep 1
mapfile -t INNER < <(grep -E '^(DISPLAY|WAYLAND_DISPLAY|DBUS_SESSION_BUS_ADDRESS)=' "$N/kwin-env")
# Refuse to go on unless display, Wayland socket and bus are the nest's own.
for v in "${INNER[@]}"; do
  case $v in
    DISPLAY=:0 | DISPLAY=:1 | "DISPLAY=${DISPLAY:-none}" | WAYLAND_DISPLAY=wayland-* | *=*/run/user/*)
      echo "refusing: $v is not the nested session" >&2; exit 3 ;;
  esac
done
[[ ${#INNER[@]} == 3 ]] || { echo "nested KWin did not start" >&2; exit 1; }
nest() { env -i "${ISO[@]}" "${INNER[@]}" "$@"; }
mapfile -t INNER_X11 < <(printf '%s\n' "${INNER[@]}" | grep -v '^WAYLAND_DISPLAY=')
nest_x11() { env -i "${ISO[@]}" "${INNER_X11[@]}" "$@"; }

mcp() { # mcp TOOL JSON: one MCP tool call through `swiftsim --mcp`
  "${MCP_NEST:-nest}" python3 -I -c '
import json, subprocess, sys
p = subprocess.Popen([sys.argv[1], "--mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def send(m): p.stdin.write(json.dumps(dict(jsonrpc="2.0", **m)) + "\n"); p.stdin.flush()
send(dict(id=1, method="initialize", params=dict(protocolVersion="2025-06-18", capabilities={}, clientInfo=dict(name="screenshots", version="1"))))
p.stdout.readline(); send(dict(method="notifications/initialized"))
send(dict(id=2, method="tools/call", params=dict(name=sys.argv[2], arguments=json.loads(sys.argv[3]))))
r = json.loads(p.stdout.readline())["result"]; p.stdin.close(); p.wait()
sys.exit(r.get("isError", False))' "$S" "$1" "$2"
}

for kind in tran ac; do
  [[ $kind == tran ]] && cmd='.tran 20m' || cmd='.ac dec 100 1 1meg'
  # The "RC low-pass filter" example, with R as a live parameter.
  f=$ROOT/rc_$kind.sws
  printf '{"format":1,"comps":[{"kind":"Voltage","name":"V1","value":"PULSE(0 1 0 1u 1u 5m 10m) AC 1","pos":[0,0],"rot":0,"mirror":false},{"kind":"Resistor","name":"R1","value":"{R}","pos":[7,-4],"rot":1,"mirror":false},{"kind":"Capacitor","name":"C1","value":"1u","pos":[14,0],"rot":0,"mirror":false},{"kind":"Ground","name":"","value":"","pos":[0,2],"rot":0,"mirror":false},{"kind":"Ground","name":"","value":"","pos":[14,2],"rot":0,"mirror":false},{"kind":"Label","name":"","value":"in","pos":[0,-4],"rot":0,"mirror":false},{"kind":"Label","name":"","value":"out","pos":[14,-4],"rot":0,"mirror":false}],"wires":[{"a":[0,-2],"b":[0,-4]},{"a":[0,-4],"b":[5,-4]},{"a":[9,-4],"b":[14,-4]},{"a":[14,-4],"b":[14,-2]}],"texts":[{"pos":[0,6],"text":"%s\\n.param R=1k"}]}' "$cmd" >"$f"
  # X11 backend (no WAYLAND_DISPLAY) so xdotool reaches the window on the nest's Xwayland.
  MCP_NEST=nest_x11 mcp open_in_gui "{\"file\":\"$f\"}"
  sleep 3
  APP=$(pgrep -f -x "$S $f")
  mcp gui_run '{"generation":1}'
  # The Probe tool is active: clicking R1 adds I(R1). The first events of a
  # fresh input connection can be lost, so move once before clicking.
  nest xdotool mousemove 640 300; sleep 1
  nest xdotool mousemove 575 123 click 1 mousemove 1270 470
  sleep 17 # the toolbar's "Driven by an agent" note lasts 15 s
  out=$REPO/docs/$([[ $kind == tran ]] && echo transient || echo ac).png
  rm -f "$out"
  nest spectacle -b -n -f -o "$out"
  for _ in $(seq 50); do [[ -s $out ]] && break; sleep 0.2; done
  kill "$APP"; APP=
  echo "wrote $out"
done
