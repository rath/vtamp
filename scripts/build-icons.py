#!/usr/bin/env python3
"""Derive the macOS, web, iPhone, and Android icons from the approved cell-mosaic artwork (macOS tools)."""
from pathlib import Path
import copy
import struct
import subprocess
import tempfile
import xml.etree.ElementTree as ET
import zlib

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "icon.svg"
RASTER = ROOT / "assets" / "icon.png"
SVG = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG)


def preserve_prompt(destination):
    # sips strips PNG text chunks. Keep the approved generation provenance on exports.
    data = destination.read_bytes()
    prompt = (ROOT / "assets" / "icon-prompt.txt").read_text().strip().encode("utf-8")
    payload = b"impeccable:prompt\0" + prompt
    chunk = b"tEXt" + payload
    encoded = struct.pack(">I", len(payload)) + chunk + struct.pack(">I", zlib.crc32(chunk))
    assert data[-12:] == bytes.fromhex("0000000049454e44ae426082")
    destination.write_bytes(data[:-12] + encoded + data[-12:])


def render(drawing, destination):
    # sips renders SVG at the drawing's own 1024 size.
    subprocess.run(["sips", "-s", "format", "png", str(drawing), "--out", str(destination)],
                   check=True, capture_output=True)


def resize(size, destination):
    subprocess.run(["sips", "-z", str(size), str(size), str(RASTER),
                    "--out", str(destination)], check=True, capture_output=True)


def group(root, name):
    return next(node for node in root.iter(f"{{{SVG}}}g") if node.get("id") == name)


def cells(root):
    """Every grid cell as (x, y, size, radius, level): 1 lit, 0.5 half lit, 0 unlit."""
    found = []

    def walk(node, fill):
        fill = node.get("fill", fill)
        if node.tag != f"{{{SVG}}}rect":
            for child in node:
                walk(child, fill)
            return
        assert node.get("width") == node.get("height"), "grid cells are square"
        level = float(node.get("opacity", "1")) if fill == "url(#phosphor-fill)" else 0.0
        found.append((float(node.get("x")), float(node.get("y")), float(node.get("width")),
                      float(node.get("rx")), level))

    walk(group(root, "terminal-grid"), None)
    assert len(found) == 81, "expected the 9 x 9 grid"
    return found


# The iPhone icon is the grid without the tile: iOS masks the icon itself, so the
# cells sit on a flat dark ground that fills the square. The ground is lighter
# than the tile, so unlit cells need more opacity to stay just visible.
IOS_GROUND = (40, 40, 40)
IOS_UNLIT_OPACITY = "0.5"


def ios_icon_svg(root):
    root = copy.deepcopy(root)
    tile = group(root, "tile")
    for node in list(tile):
        tile.remove(node)
    ground = ET.SubElement(root.find(f"{{{SVG}}}defs"), f"{{{SVG}}}linearGradient",
                           {"id": "ground", "x1": "0", "y1": "0", "x2": "0", "y2": "1"})
    ET.SubElement(ground, f"{{{SVG}}}stop", {"offset": "0", "stop-color": "#2c2c2c"})
    ET.SubElement(ground, f"{{{SVG}}}stop", {"offset": "1", "stop-color": "#242424"})
    ET.SubElement(tile, f"{{{SVG}}}rect", {"width": "1024", "height": "1024", "fill": "url(#ground)"})
    for node in group(root, "terminal-grid").iter(f"{{{SVG}}}g"):
        if node.get("fill") == "#46513f":
            node.set("opacity", IOS_UNLIT_OPACITY)
    return ET.tostring(root, encoding="unicode")


def png_rows(data):
    """Unfiltered rows of an 8-bit RGBA, non-interlaced PNG such as sips writes."""
    width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", data[16:29])
    assert (depth, color, interlace) == (8, 6, 0), "expected 8-bit RGBA without interlacing"
    pos, compressed = 8, b""
    while pos < len(data):
        length, = struct.unpack(">I", data[pos:pos + 4])
        if data[pos + 4:pos + 8] == b"IDAT":
            compressed += data[pos + 8:pos + 8 + length]
        pos += 12 + length
    raw, stride, rows, previous = zlib.decompress(compressed), width * 4, [], bytearray(width * 4)
    for y in range(height):
        start = y * (stride + 1)
        kind, line = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        for x in range(stride):
            left = line[x - 4] if x >= 4 else 0
            up, corner = previous[x], previous[x - 4] if x >= 4 else 0
            if kind == 1:
                line[x] = (line[x] + left) & 255
            elif kind == 2:
                line[x] = (line[x] + up) & 255
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 255
            elif kind == 4:
                guess = left + up - corner
                pa, pb, pc = abs(guess - left), abs(guess - up), abs(guess - corner)
                line[x] = (line[x] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 255
        rows.append(line)
        previous = line
    return width, height, rows


def flatten(destination, fill):
    """Composite over `fill` and drop the alpha channel, as iOS icons require."""
    width, height, rows = png_rows(destination.read_bytes())
    out = bytearray()
    for line in rows:
        out.append(0)
        for x in range(0, width * 4, 4):
            alpha = line[x + 3]
            out.extend((line[x + c] * alpha + fill[c] * (255 - alpha) + 127) // 255 for c in range(3))
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    destination.write_bytes(b"\x89PNG\r\n\x1a\n"
                            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                            + chunk(b"IDAT", zlib.compress(bytes(out), 9))
                            + chunk(b"IEND", b""))


def ios_icon(root, destination):
    with tempfile.TemporaryDirectory(prefix="vtamp-ios-icon-") as directory:
        drawing = Path(directory) / "icon.svg"
        drawing.write_text(ios_icon_svg(root))
        render(drawing, destination)
    flatten(destination, IOS_GROUND)


# The Android launcher foreground is the grid alone in the 108 dp adaptive-icon
# viewport, scaled so its corners stay inside the 66 dp safe circle. One lime
# drawable serves the colour and the monochrome layer: cell levels become alpha.
ANDROID_SCALE = 0.08
ANDROID_ALPHA = {1.0: "1", 0.5: "0.5", 0.0: "0.1"}


def android_icon(root, destination):
    def cell(x, y, size, radius):
        x, y = 54 + (x - 512) * ANDROID_SCALE, 54 + (y - 512) * ANDROID_SCALE
        size, r = size * ANDROID_SCALE, radius * ANDROID_SCALE
        side = size - 2 * r
        return (f"M{x + r:.2f},{y:.2f}h{side:.2f}a{r:.2f},{r:.2f} 0 0 1 {r:.2f},{r:.2f}"
                f"v{side:.2f}a{r:.2f},{r:.2f} 0 0 1 -{r:.2f},{r:.2f}"
                f"h-{side:.2f}a{r:.2f},{r:.2f} 0 0 1 -{r:.2f},-{r:.2f}"
                f"v-{side:.2f}a{r:.2f},{r:.2f} 0 0 1 {r:.2f},-{r:.2f}Z")

    grid = cells(root)
    assert {level for *_, level in grid} == set(ANDROID_ALPHA), "expected lit, half-lit, and unlit cells"
    paths = "".join(
        f'    <path android:fillColor="#B4F676" android:fillAlpha="{alpha}" android:pathData="'
        + " ".join(cell(x, y, size, radius) for x, y, size, radius, found in grid if found == level)
        + '" />\n'
        for level, alpha in ANDROID_ALPHA.items())
    destination.write_text(
        "<!-- Generated by scripts/build-icons.py from assets/icon.svg. -->\n"
        '<vector xmlns:android="http://schemas.android.com/apk/res/android" android:width="108dp"'
        ' android:height="108dp" android:viewportWidth="108" android:viewportHeight="108">\n'
        f"{paths}</vector>\n")


def main():
    root = ET.parse(SOURCE).getroot()
    render(SOURCE, RASTER)
    preserve_prompt(RASTER)
    for size, name in [(32, "favicon-32.png"), (64, "favicon-64.png"),
                       (128, "mark.png"), (180, "apple-touch-icon.png")]:
        destination = ROOT / "site" / name
        resize(size, destination)
        preserve_prompt(destination)
    with tempfile.TemporaryDirectory(prefix="vtamp-icons-") as directory:
        iconset = Path(directory) / "vtamp.iconset"
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            resize(size, iconset / f"icon_{size}x{size}.png")
            resize(size * 2, iconset / f"icon_{size}x{size}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset),
                        "-o", str(ROOT / "assets" / "vtamp.icns")], check=True)
    ios_icon(root, ROOT / "ios" / "Sources" / "App" / "Assets.xcassets" / "AppIcon.appiconset" / "icon-1024.png")
    android_icon(root, ROOT / "android" / "app" / "src" / "main" / "res" / "drawable" / "ic_mosaic.xml")
    print("Updated assets/icon.png, web icons, assets/vtamp.icns, and the iOS and Android app icons")


if __name__ == "__main__":
    main()
