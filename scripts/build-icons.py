#!/usr/bin/env python3
"""Derive web and macOS icons from the approved V-meter artwork and draw the iPhone icon (macOS tools)."""
from pathlib import Path
import math
import struct
import subprocess
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "icon.png"


def preserve_prompt(destination):
    # sips strips PNG text chunks. Keep the approved generation provenance on exports.
    data = destination.read_bytes()
    prompt = (ROOT / "assets" / "icon-prompt.txt").read_text().strip().encode("utf-8")
    payload = b"impeccable:prompt\0" + prompt
    chunk = b"tEXt" + payload
    encoded = struct.pack(">I", len(payload)) + chunk + struct.pack(">I", zlib.crc32(chunk))
    assert data[-12:] == bytes.fromhex("0000000049454e44ae426082")
    destination.write_bytes(data[:-12] + encoded + data[-12:])


def resize(size, destination):
    subprocess.run(["sips", "-z", str(size), str(size), str(SOURCE),
                    "--out", str(destination)], check=True, capture_output=True)


# The iPhone icon redraws the V-meter for the home screen: the five lit bars on
# a flat dark ground with margins. Each bar's left edge, right edge, and top are
# traced from the approved artwork in a 1024 frame; one 45-degree V cuts every bar.
IOS_BARS = [(91, 240, 127), (262, 412, 330), (440, 584, 465), (613, 763, 330), (785, 935, 127)]
IOS_AXIS = 513  # the V's vertical axis in the traced frame
IOS_CUT = 430  # bar bottoms: y = x + IOS_CUT left of the axis, mirrored right
IOS_SCALE = 0.707  # the traced frame shrinks around IOS_ORIGIN,
IOS_ORIGIN = (513, 526)  # which lands on the icon's centre
IOS_GROUND = (40, 40, 40)


def ios_bar(index):
    """Bar `index` as a clockwise polygon, cut by the V."""
    left, right, top = IOS_BARS[index]

    def bottom(x):
        return IOS_CUT + min(x, 2 * IOS_AXIS - x)

    corners = [(left, top), (right, top), (right, bottom(right))]
    if left < IOS_AXIS < right:
        corners.append((IOS_AXIS, bottom(IOS_AXIS)))
    return corners + [(left, bottom(left))]


def placed(points):
    ox, oy = IOS_ORIGIN
    return [(512 + (x - ox) * IOS_SCALE, 512 + (y - oy) * IOS_SCALE) for x, y in points]


def rounded(points, radius, paint, attributes=""):
    """A convex clockwise polygon as one path whose corners are arcs of `radius`."""
    points = placed(points)
    path = []
    for index, (px, py) in enumerate(points):
        (ax, ay), (bx, by) = points[index - 1], points[(index + 1) % len(points)]
        ua, ub = math.hypot(ax - px, ay - py), math.hypot(bx - px, by - py)
        cosine = ((ax - px) * (bx - px) + (ay - py) * (by - py)) / (ua * ub)
        reach = radius / math.tan(math.acos(cosine) / 2)
        start = (px + (ax - px) / ua * reach, py + (ay - py) / ua * reach)
        end = (px + (bx - px) / ub * reach, py + (by - py) / ub * reach)
        path.append(f"{'M' if index == 0 else 'L'}{start[0]:.2f},{start[1]:.2f}"
                    f"A{radius},{radius} 0 0 1 {end[0]:.2f},{end[1]:.2f}")
    return f'<path d="{"".join(path)}Z" fill="{paint}" {attributes}/>'


def ios_icon_svg():
    def blur(name, deviation):
        return (f'<filter id="{name}" filterUnits="userSpaceOnUse" x="0" y="0" width="1024" height="1024">'
                f'<feGaussianBlur stdDeviation="{deviation}"/></filter>')

    defs = (
        '<linearGradient id="ground" x1="0" y1="0" x2="0" y2="1">'
        '<stop offset="0" stop-color="#2c2c2c"/><stop offset="1" stop-color="#242424"/></linearGradient>'
        '<linearGradient id="phosphor" x1="0" y1="0" x2="0" y2="1">'
        '<stop offset="0" stop-color="#ecffb0"/><stop offset="0.4" stop-color="#c2f77f"/>'
        '<stop offset="1" stop-color="#7fbf4a"/></linearGradient>'
        + blur("halo", 40) + blur("glow", 10))
    bars = [ios_bar(index) for index in range(len(IOS_BARS))]
    body = (['<rect width="1024" height="1024" fill="url(#ground)"/>']
            + [rounded(bar, 10, "#b4f676", 'filter="url(#halo)" opacity="0.22"') for bar in bars]
            + [rounded(bar, 10, "#b4f676", 'filter="url(#glow)" opacity="0.7"') for bar in bars]
            + [rounded(bar, 10, "url(#phosphor)") for bar in bars])
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">'
            f'<defs>{defs}</defs>{"".join(body)}</svg>')


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


def ios_icon(destination):
    # sips renders SVG, filters included, at the drawing's own 1024 size.
    with tempfile.TemporaryDirectory(prefix="vtamp-ios-icon-") as directory:
        drawing = Path(directory) / "icon.svg"
        drawing.write_text(ios_icon_svg())
        subprocess.run(["sips", "-s", "format", "png", str(drawing), "--out", str(destination)],
                       check=True, capture_output=True)
    flatten(destination, IOS_GROUND)


def main():
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
    ios_icon(ROOT / "ios" / "Sources" / "App" / "Assets.xcassets" / "AppIcon.appiconset" / "icon-1024.png")
    print("Updated web icons, assets/vtamp.icns, and the iOS app icon")


if __name__ == "__main__":
    main()
