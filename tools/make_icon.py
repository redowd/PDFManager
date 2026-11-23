"""生成 PDF 管理器的应用图标（多尺寸 .ico + 窗口图标原始 RGBA）。

设计：深蓝灰圆角底 + 白色文档页 + 右上折角 + 三条正文短线。
纯标准库实现（无 Pillow）：4x 超采样抗锯齿，PNG 用 zlib 手写，ICO 小尺寸用 32 位 BMP、
大尺寸用 PNG 内嵌，兼容 Windows 资源管理器/任务栏/标题栏。

用法：python tools/make_icon.py
产物：
  rust-backend/assets/app.ico
  rust-backend/assets/icon-256.png
  rust-backend/assets/icon_32.rgba   （窗口图标，32x32 RGBA 原始字节）
"""

import os
import struct
import zlib

OUT_DIR = os.path.join(os.path.dirname(__file__), "..", "rust-backend", "assets")

BG = (0x22, 0x38, 0x4A)        # 深蓝灰底
PAGE = (0xFF, 0xFF, 0xFF)      # 文档页
FOLD = (0x8F, 0xB3, 0xD0)      # 折角
LINE = (0x2E, 0x5C, 0x8A)      # 正文线条

SS = 4  # 超采样倍数


def _in_round_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def _in_tri(x, y, ax, ay, bx, by, cx, cy):
    d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by)
    d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy)
    d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay)
    has_neg = d1 < 0 or d2 < 0 or d3 < 0
    has_pos = d1 > 0 or d2 > 0 or d3 > 0
    return not (has_neg and has_pos)


def render(size):
    """返回 size*size*4 的 RGBA 字节（非预乘）。"""
    pixels = bytearray(size * size * 4)
    lines = [0.42, 0.53, 0.64] if size >= 32 else [0.45, 0.57]
    line_h = 0.030 if size >= 32 else 0.045
    step = 1.0 / (size * SS)
    for py in range(size):
        for px in range(size):
            r = g = b = a = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = (px * SS + sx + 0.5) * step
                    y = (py * SS + sy + 0.5) * step
                    color = None
                    # 背景圆角方块
                    if _in_round_rect(x, y, 0.02, 0.02, 0.98, 0.98, 0.17):
                        color = BG
                        # 文档页（右上角被斜切）
                        in_cut = x > 0.60 and y < 0.30 and (x - 0.60) > (y - 0.17)
                        if _in_round_rect(x, y, 0.27, 0.17, 0.73, 0.83, 0.035) and not in_cut:
                            color = PAGE
                            # 折角
                            if x >= 0.595 and y <= 0.305 and (x - 0.60) <= (y - 0.17):
                                color = FOLD
                            else:
                                for ly in lines:
                                    if (
                                        ly <= y <= ly + line_h
                                        and 0.35 <= x <= 0.65
                                    ):
                                        color = LINE
                                        break
                    if color is None:
                        continue
                    r += color[0]
                    g += color[1]
                    b += color[2]
                    a += 255.0
            total = float(SS * SS)
            cover = a / total
            if cover <= 0:
                continue
            # 颜色按覆盖率归一，alpha 单独记录
            idx = (py * size + px) * 4
            pixels[idx] = int(round(r / total / (cover / 255.0) if cover else 0))
            pixels[idx + 1] = int(round(g / total / (cover / 255.0) if cover else 0))
            pixels[idx + 2] = int(round(b / total / (cover / 255.0) if cover else 0))
            pixels[idx + 3] = int(round(cover))
    return bytes(pixels)


def png_bytes(size, rgba):
    raw = bytearray()
    stride = size * 4
    for y in range(size):
        raw.append(0)  # filter: None
        raw.extend(rgba[y * stride:(y + 1) * stride])

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def bmp_payload(size, rgba):
    header = struct.pack(
        "<IiiHHIIiiII",
        40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0
    )
    body = bytearray()
    stride = size * 4
    for y in range(size - 1, -1, -1):
        row = rgba[y * stride:(y + 1) * stride]
        for x in range(size):
            r, g, b, a = row[x * 4:x * 4 + 4]
            body.extend((b, g, r, a))
    mask_stride = ((size + 31) // 32) * 4
    return header + bytes(body) + b"\x00" * (mask_stride * size)


def build_ico(sizes):
    images = []
    for size in sizes:
        rgba = render(size)
        if size >= 128:
            images.append((size, png_bytes(size, rgba)))
        else:
            images.append((size, bmp_payload(size, rgba)))
    count = len(images)
    header = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    entries = b""
    data = b""
    for size, payload in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,
            0,
            1,
            32,
            len(payload),
            offset,
        )
        data += payload
        offset += len(payload)
    return header + entries + data


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    ico = build_ico(sizes)
    with open(os.path.join(OUT_DIR, "app.ico"), "wb") as f:
        f.write(ico)
    rgba256 = render(256)
    with open(os.path.join(OUT_DIR, "icon-256.png"), "wb") as f:
        f.write(png_bytes(256, rgba256))
    with open(os.path.join(OUT_DIR, "icon_32.rgba"), "wb") as f:
        f.write(render(32))
    print("icon bytes:", len(ico), "sizes:", sizes)


if __name__ == "__main__":
    main()
