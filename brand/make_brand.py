#!/usr/bin/env python3
"""Generate GPN brand assets (avatar, logo, banner) with PIL.

Mark: hexagonal block outline in cyan with a bold white "G" inside, plus a node dot
at the lower-right vertex — reads at 48 px, no gradients that muddy on small screens.

Outputs into brand/:
  gpn-logo-1024.png   master mark on transparent
  gpn-avatar-400.png  X profile picture (square, dark tile)
  gpn-banner-1500x500.png
  gpn-logo.svg        vector version for later editing
"""
import os
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "brand")
os.makedirs(OUT, exist_ok=True)

BG      = (11, 18, 32, 255)      # #0B1220
BG_EDGE = (18, 28, 48, 255)
CYAN    = (34, 211, 238, 255)    # #22D3EE
WHITE   = (248, 250, 252, 255)
MUTED   = (148, 163, 184, 255)
FONT_B  = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
FONT_R  = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"

SS = 4  # supersample factor


def hexagon(cx, cy, r):
    """Flat-top hexagon points."""
    import math
    pts = []
    for i in range(6):
        a = math.radians(60 * i - 30)
        pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts


def draw_mark(img_size=1024, tile=True, pad=0.12):
    S = img_size * SS
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    if tile:
        m = int(S * 0.02)
        d.rounded_rectangle([m, m, S - m, S - m], radius=int(S * 0.22), fill=BG,
                            outline=BG_EDGE, width=max(2, S // 340))

    c = S // 2
    r = int(S * (0.5 - pad))
    stroke = max(6, int(S * 0.035))
    d.polygon(hexagon(c, c, r), outline=CYAN, width=stroke)

    # inner "G"
    f = ImageFont.truetype(FONT_B, int(r * 1.05))
    tb = d.textbbox((0, 0), "G", font=f)
    tw, th = tb[2] - tb[0], tb[3] - tb[1]
    d.text((c - tw / 2 - tb[0], c - th / 2 - tb[1]), "G", font=f, fill=WHITE)

    # node dot at the lower-right vertex
    import math
    a = math.radians(60 * 4 - 30)  # vertex index 4 = lower-right
    nx, ny = c + r * math.cos(a), c + r * math.sin(a)
    dr = max(7, int(S * 0.028))
    d.ellipse([nx - dr * 1.9, ny - dr * 1.9, nx + dr * 1.9, ny + dr * 1.9], fill=BG)
    d.ellipse([nx - dr, ny - dr, nx + dr, ny + dr], fill=CYAN)

    return img.resize((img_size, img_size), Image.LANCZOS)


def avatar(size=400):
    return draw_mark(size, tile=True, pad=0.14)


def logo(size=1024):
    return draw_mark(size, tile=True, pad=0.12)


def banner(w=1500, h=500):
    W, H = w * SS, h * SS
    img = Image.new("RGBA", (W, H), BG)
    d = ImageDraw.Draw(img)

    # subtle vignette bands
    for i in range(H):
        t = i / H
        d.line([(0, i), (W, i)], fill=(int(11 + 6 * t), int(18 + 8 * t), int(32 + 12 * t), 255))

    # mark on the left
    mark = draw_mark(int(H * 0.62), tile=False)
    ms = mark.size[0]
    img.alpha_composite(mark, (int(W * 0.05), (H - ms) // 2))

    f_title = ImageFont.truetype(FONT_B, int(H * 0.24))
    f_tag = ImageFont.truetype(FONT_R, int(H * 0.075))

    tx = int(W * 0.05) + ms + int(W * 0.035)
    ty = H // 2 - int(H * 0.20)
    d.text((tx, ty), "GPN", font=f_title, fill=WHITE)
    d.text((tx, ty + int(H * 0.26)), "Mine when idle. Serve when asked.", font=f_tag, fill=CYAN)
    d.text((tx, ty + int(H * 0.38)), "gpncoin.ai", font=f_tag, fill=MUTED)

    return img.resize((w, h), Image.LANCZOS)


def write_svg(path):
    svg = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <rect x="10" y="10" width="492" height="492" rx="112" fill="#0B1220" stroke="#12203A" stroke-width="4"/>
  <polygon points="256,74 413,165 413,347 256,438 99,347 99,165"
           fill="none" stroke="#22D3EE" stroke-width="18" stroke-linejoin="round"/>
  <text x="256" y="318" font-family="DejaVu Sans, Arial, Helvetica, sans-serif" font-size="230"
        font-weight="700" fill="#F8FAFC" text-anchor="middle">G</text>
  <circle cx="366" cy="400" r="26" fill="#0B1220"/>
  <circle cx="366" cy="400" r="15" fill="#22D3EE"/>
</svg>
"""
    with open(path, "w") as f:
        f.write(svg)


if __name__ == "__main__":
    avatar(400).save(os.path.join(OUT, "gpn-avatar-400.png"))
    logo(1024).save(os.path.join(OUT, "gpn-logo-1024.png"))
    banner(1500, 500).save(os.path.join(OUT, "gpn-banner-1500x500.png"))
    write_svg(os.path.join(OUT, "gpn-logo.svg"))
    for f in sorted(os.listdir(OUT)):
        p = os.path.join(OUT, f)
        print(f"{f:<28} {os.path.getsize(p):>9,} bytes")
