"""Рисует значок OmniFlex (res/app.ico): тёмный квадрат со скруглёнными углами, пилюля-остров и полоски громкости.

Только стандартная библиотека: PNG пишется вручную, сглаживание — суперсэмплингом.
Запуск: python tools/make_icon.py
"""
import math
import os
import struct
import zlib

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
SS = 4  # суперсэмплинг


def rounded_rect(x, y, x0, y0, x1, y1, r):
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r and x0 <= x <= x1 and y0 <= y <= y1


def pixel(u, v):
    """u, v — 0..1. Возвращает RGBA (0..1) без премультипликации."""
    # Фон: скруглённый квадрат с вертикальным градиентом
    if not rounded_rect(u, v, 0.02, 0.02, 0.98, 0.98, 0.22):
        return (0, 0, 0, 0)
    t = v
    top = (0.36, 0.42, 1.00)
    bottom = (0.62, 0.30, 0.95)
    col = tuple(top[i] * (1 - t) + bottom[i] * t for i in range(3))
    # Пилюля-остров
    if rounded_rect(u, v, 0.20, 0.17, 0.80, 0.37, 0.10):
        return (0.05, 0.05, 0.07, 1)
    # Точки на пилюле: камера и индикатор
    if (u - 0.70) ** 2 + (v - 0.27) ** 2 <= 0.035 ** 2:
        return (0.35, 0.95, 0.55, 1)
    # Полоски громкости
    bars = [(0.24, 0.62), (0.38, 0.50), (0.52, 0.56), (0.66, 0.46)]
    for bx, by in bars:
        if rounded_rect(u, v, bx, by, bx + 0.10, 0.84, 0.05):
            return (1, 1, 1, 1)
    return col + (1,)


def render(size):
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(SS):
                for sx in range(SS):
                    u = (x + (sx + 0.5) / SS) / size
                    v = (y + (sy + 0.5) / SS) / size
                    r, g, b, a = pixel(u, v)
                    acc[0] += r * a
                    acc[1] += g * a
                    acc[2] += b * a
                    acc[3] += a
            n = SS * SS
            a = acc[3] / n
            if a > 0:
                r, g, b = (acc[i] / n / a for i in range(3))
            else:
                r = g = b = 0
            row += bytes([round(r * 255), round(g * 255), round(b * 255), round(a * 255)])
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def main():
    images = [render(s) for s in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = b""
    for size, data in zip(SIZES, images):
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out = os.path.join(os.path.dirname(__file__), "..", "res", "app.ico")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "wb") as f:
        f.write(header + entries + b"".join(images))
    print("written", os.path.abspath(out))


if __name__ == "__main__":
    main()
