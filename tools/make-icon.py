"""Значок MH Files: папка цвета акцента с тремя столбиками семейства MH на тёмной плитке.

Собирает assets/icon/MH-Files.ico (16–256 px). Та же раскладка нарисована кодом в
crates/ui/src/main.rs (значок окна) и crates/ui/src/icons.rs (логотип в заголовке):
меняя фигуры здесь, поменяйте и там.

    python3 tools/make-icon.py
"""

from pathlib import Path

from PIL import Image

BACKGROUND = (0x12, 0x16, 0x1D)
ACCENT = (0x3F, 0xD0, 0xD8)
# Плитка 64×64 со скруглением 12; фигуры — (left, top, right, bottom, радиус, доля акцента).
TILE_RADIUS = 12.0
SHAPES = [
    (8, 11, 28, 22, 3, 0.55),  # язычок папки
    (8, 16, 56, 53, 3, 0.55),  # задняя стенка
    (8, 23, 56, 53, 3, 1.0),  # передняя стенка
    (19, 39, 25, 47, 1, 0.0),  # столбики MH — прорезями в передней стенке
    (29, 31, 35, 47, 1, 0.0),
    (39, 36, 45, 47, 1, 0.0),
]


def in_rounded(x, y, left, top, right, bottom, radius):
    if not (left <= x < right and top <= y < bottom):
        return False
    cx = min(max(x, left + radius), right - radius)
    cy = min(max(y, top + radius), bottom - radius)
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius


def shade(x, y):
    """Доля акцента в точке (координаты плитки 0..64); `None` — вне плитки."""
    if not in_rounded(x, y, 0, 0, 64, 64, TILE_RADIUS):
        return None
    value = 0.0
    for left, top, right, bottom, radius, share in SHAPES:
        if in_rounded(x, y, left, top, right, bottom, radius):
            value = share
    return value


def render(size, samples=8):
    image = Image.new("RGBA", (size, size))
    scale = 64.0 / size
    for py in range(size):
        for px in range(size):
            tile = 0
            mixed = 0.0
            for sy in range(samples):
                for sx in range(samples):
                    x = (px + (sx + 0.5) / samples) * scale
                    y = (py + (sy + 0.5) / samples) * scale
                    value = shade(x, y)
                    if value is not None:
                        tile += 1
                        mixed += value
            share = mixed / tile if tile else 0.0
            color = tuple(
                round(b * (1 - share) + a * share) for a, b in zip(ACCENT, BACKGROUND)
            )
            image.putpixel((px, py), color + (round(tile / samples**2 * 255),))
    return image


def main():
    sizes = [256, 128, 64, 48, 32, 24, 16]
    images = [render(size) for size in sizes]
    out = Path(__file__).resolve().parent.parent / "assets" / "icon" / "MH-Files.ico"
    images[0].save(out, sizes=[(s, s) for s in sizes], append_images=images[1:])
    print(out)


if __name__ == "__main__":
    main()
