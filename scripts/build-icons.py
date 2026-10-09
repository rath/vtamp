#!/usr/bin/env python3
"""Derive web, macOS, and iOS icons from the approved V-meter artwork (macOS tools)."""
from pathlib import Path
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


# The opaque tile inside the macOS artwork's transparent margin, inset past its
# rim. iOS masks the square with a larger corner radius than the tile's, so
# the filled corners never show.
IOS_TILE = {"top": 98, "left": 103, "side": 1047}
IOS_FILL = (24, 30, 26)


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
    with tempfile.TemporaryDirectory(prefix="vtamp-ios-icon-") as directory:
        tile = Path(directory) / "tile.png"
        side = str(IOS_TILE["side"])
        subprocess.run(["sips", "-c", side, side, "--cropOffset", str(IOS_TILE["top"]),
                        str(IOS_TILE["left"]), str(SOURCE), "--out", str(tile)],
                       check=True, capture_output=True)
        subprocess.run(["sips", "-z", "1024", "1024", str(tile), "--out", str(destination)],
                       check=True, capture_output=True)
    flatten(destination, IOS_FILL)
    preserve_prompt(destination)


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
