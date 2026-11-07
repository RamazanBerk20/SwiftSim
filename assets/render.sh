#!/bin/sh
# Regenerates the PNG and ICO icons from swiftsim.svg (needs rsvg-convert and Python with Pillow).
set -e
cd "$(dirname "$0")"
mkdir -p icons
for s in 16 24 32 48 64 128 256 512; do rsvg-convert -w $s -h $s swiftsim.svg -o icons/$s.png; done
python3 -c "
from PIL import Image
ims = [Image.open(f'icons/{s}.png') for s in (256, 128, 64, 48, 32, 24, 16)]
ims[0].save('swiftsim.ico', sizes=[i.size for i in ims], append_images=ims[1:])"
