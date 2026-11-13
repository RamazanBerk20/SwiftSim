#!/bin/sh
# Installs SwiftSim with menu entry, icon and .sws/.asc file types.
# Default is per-user (~/.local); for all users: sudo PREFIX=/usr/local ./install.sh
set -eu
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
install -Dm755 swiftsim "$PREFIX/bin/swiftsim"
mkdir -p "$PREFIX/share/applications"
# Exec= quoting per the desktop-entry spec: the path in double quotes with " ` $ \ escaped, then every \ doubled
exe=$(printf '%s' "$PREFIX/bin/swiftsim" | sed 's/[\\"`$]/\\&/g; s/\\/\\\\/g')
EXEC="\"$exe\"" awk '/^Exec=swiftsim/ { print "Exec=" ENVIRON["EXEC"] substr($0, 14); next } { print }' swiftsim.desktop > "$PREFIX/share/applications/swiftsim.desktop"
install -Dm644 swiftsim.xml "$PREFIX/share/mime/packages/swiftsim.xml"
install -Dm644 swiftsim.svg "$PREFIX/share/icons/hicolor/scalable/apps/swiftsim.svg"
for f in icons/*.png; do
    s=$(basename "$f" .png)
    install -Dm644 "$f" "$PREFIX/share/icons/hicolor/${s}x$s/apps/swiftsim.png"
done
update-mime-database "$PREFIX/share/mime" >/dev/null 2>&1 || true
update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
echo "SwiftSim installed to $PREFIX"
