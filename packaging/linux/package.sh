#!/bin/sh
# Builds dist/swiftsim-linux-x86_64.tar.gz and dist/SwiftSim-x86_64.AppImage from target/release/swiftsim.
# Usage: packaging/linux/package.sh path/to/appimagetool
set -eu
cd "$(dirname "$0")/../.."
s=target/linux-package
rm -rf "$s" && mkdir -p "$s" dist

t=$s/swiftsim-linux-x86_64
mkdir -p "$t/icons"
cp target/release/swiftsim packaging/linux/install.sh packaging/linux/swiftsim.desktop packaging/linux/swiftsim.xml \
   assets/swiftsim.svg LICENSE-MIT LICENSE-APACHE LICENSE-OFL README.md "$t/"
cp assets/icons/*.png "$t/icons/"
tar -C "$s" -czf dist/swiftsim-linux-x86_64.tar.gz swiftsim-linux-x86_64

a=$s/AppDir
mkdir -p "$a/usr/bin" "$a/usr/share/mime/packages"
cp target/release/swiftsim "$a/usr/bin/"
cp packaging/linux/swiftsim.desktop "$a/"
cp assets/icons/256.png "$a/swiftsim.png"
cp packaging/linux/swiftsim.xml "$a/usr/share/mime/packages/"
ln -s usr/bin/swiftsim "$a/AppRun"
"$1" "$a" dist/SwiftSim-x86_64.AppImage
