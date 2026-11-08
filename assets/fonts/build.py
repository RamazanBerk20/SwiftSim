#!/usr/bin/env python3
"""Builds assets/fonts/fallback.bin, the last-resort fonts compiled into SwiftSim.

Needs fontTools, Pillow and the Noto fonts of a Linux distribution (Arch: noto-fonts, noto-fonts-cjk,
noto-fonts-emoji).
Usage: python3 -I assets/fonts/build.py [noto dir] [noto-cjk dir]

Format: u32 LE index length, gzip of a JSON index, then one gzip per font. Each index entry is
{"name", "at", "len", "cover": [[first, last], ...]}, with offsets from the end of the index.
The emoji font is stored as it is: its images are PNGs already, and it is used without unpacking.
"""
import glob, gzip, io, json, os, struct, subprocess, sys, tempfile, unicodedata
from fontTools.ttLib import TTCollection, TTFont
from PIL import Image

noto = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/noto"
cjk = sys.argv[2] if len(sys.argv) > 2 else "/usr/share/fonts/noto-cjk"
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fallback.bin")

# No mono/display/UI duplicates or SignWriting (1.5 MB, needs its own layout). Symbols stay: the toolbar uses them.
SKIP = ("Mono", "Display", "UI-", "Unjoined", "Looped", "Music", "SignWriting")
CJK_RANGES = "U+2E80-2FDF,U+3000-312F,U+3130-318F,U+31F0-31FF,U+3400-4DBF,U+4E00-9FFF,U+A960-A97F,U+AC00-D7FF,U+1100-11FF,U+F900-FAFF,U+FF00-FFEF"


def cover(font):
    points, ranges = sorted(font.getBestCmap()), []
    for p in points:
        if ranges and ranges[-1][1] == p - 1:
            ranges[-1][1] = p
        else:
            ranges.append([p, p])
    return ranges


def cjk_subset():
    with tempfile.TemporaryDirectory() as d:
        face, small = os.path.join(d, "jp.otf"), os.path.join(d, "s.otf")
        TTCollection(os.path.join(cjk, "NotoSansCJK-Regular.ttc")).fonts[0].save(face)
        subprocess.run(["pyftsubset", face, f"--unicodes={CJK_RANGES}", "--no-hinting", "--layout-features=ccmp,locl", "--name-IDs=*", "--notdef-outline", f"--output-file={small}"], check=True)
        return open(small, "rb").read()


def color_emoji():
    """Noto Color Emoji at 48 px images (sharp at any UI size), down from 136 px: 4.7 MB instead of 10.7."""
    font = TTFont(os.path.join(noto, "NotoColorEmoji.ttf"))
    scale = 48 / 136
    for glyph in font["CBDT"].strikeData[0].values():
        image = Image.open(io.BytesIO(glyph.imageData)).convert("RGBA")
        width, height = max(1, round(image.width * scale)), max(1, round(image.height * scale))
        small = image.resize((width, height), Image.LANCZOS).quantize(colors=96, method=Image.FASTOCTREE, dither=Image.NONE)
        png = io.BytesIO()
        small.save(png, "PNG", optimize=True)
        glyph.imageData = png.getvalue()
        m = glyph.metrics
        m.width, m.height, m.BearingX, m.BearingY, m.Advance = width, height, round(m.BearingX * scale), round(m.BearingY * scale), round(m.Advance * scale)
    size = font["CBLC"].strikes[0].bitmapSizeTable
    size.ppemX = size.ppemY = round(size.ppemX * scale)
    for line in (size.hori, size.vert):
        line.ascender, line.descender, line.widthMax = round(line.ascender * scale), round(line.descender * scale), round(line.widthMax * scale)
    # the toolbar's play symbol stays a plain symbol; this font comes before the symbol fonts
    for table in font["cmap"].tables:
        if table.format == 14:
            table.uvsDict = {vs: [m for m in maps if m[0] != 0x25B6] for vs, maps in table.uvsDict.items()}
        else:
            table.cmap.pop(0x25B6, None)
    out = io.BytesIO()
    font.save(out)
    return out.getvalue()


# Latin/Greek/Cyrillic first, then each script's own font, largest first; symbol fonts last.
files = [os.path.join(noto, "NotoSans-Regular.ttf")]
others = (p for p in glob.glob(os.path.join(noto, "NotoSans?*-Regular.ttf")) if not any(s in os.path.basename(p) for s in SKIP))
files += sorted(others, key=lambda p: ("Math" in p or "Symbols" in p, -len(TTFont(p).getBestCmap())))
fonts = [(os.path.basename(p)[:-4], open(p, "rb").read()) for p in files]
fonts.insert(1, ("NotoSansCJKjp-Regular-subset", cjk_subset()))
fonts.append(("NotoColorEmoji-48px", color_emoji()))



def without_borrowed_letters(data, owned):
    """Removes letters another font owns. egui takes the first font in its chain that has a glyph, so
    a font that borrows a few letters would split a word between two fonts and break its shaping."""
    font = TTFont(io.BytesIO(data))
    borrowed = {c for c in font.getBestCmap() if c in owned and unicodedata.category(chr(c))[0] in "LM"}
    if borrowed:
        for table in font["cmap"].tables:
            if table.format != 14:
                table.cmap = {c: g for c, g in table.cmap.items() if c not in borrowed}
        out = io.BytesIO()
        font.save(out)
        data = out.getvalue()
    owned.update(font.getBestCmap())
    return data


owned = set()
fonts = [(name, without_borrowed_letters(data, owned)) for name, data in fonts]

index, blobs, at = [], [], 0
for name, data in fonts:
    z = data if "Emoji" in name else gzip.compress(data, 9, mtime=0)
    with tempfile.NamedTemporaryFile(suffix=".otf") as t:
        t.write(data)
        t.flush()
        index.append({"name": name, "at": at, "len": len(z), "cover": cover(TTFont(t.name))})
    blobs.append(z)
    at += len(z)

head = gzip.compress(json.dumps(index, separators=(",", ":")).encode(), 9, mtime=0)
with open(out, "wb") as f:
    f.write(struct.pack("<I", len(head)) + head + b"".join(blobs))
print(f"{len(fonts)} fonts, {os.path.getsize(out) / 1e6:.1f} MB -> {out}")
