#!/usr/bin/env python3
"""Render a stat-card image for the "why is your GPU idling" post."""
import os
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "brand")
FONT_B = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
FONT_R = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"

BG, BG2 = (11, 18, 32), (16, 26, 44)
CYAN, WHITE, MUTED = (34, 211, 238), (248, 250, 252), (148, 163, 184)
SS = 2


def card(w=1200, h=675, path="x-idle-gpu.png"):
    W, H = w * SS, h * SS
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)

    # vertical wash + a soft cyan glow bottom-left
    for y in range(H):
        t = y / H
        d.line([(0, y), (W, y)], fill=(int(BG[0] + (BG2[0] - BG[0]) * t),
                                       int(BG[1] + (BG2[1] - BG[1]) * t),
                                       int(BG[2] + (BG2[2] - BG[2]) * t)))
    d.ellipse([-W * 0.25, H * 0.55, W * 0.45, H * 1.35], fill=(14, 60, 76))

    # accent bar
    d.rectangle([int(W * 0.06), int(H * 0.10), int(W * 0.06) + int(W * 0.012), int(H * 0.90)], fill=CYAN)

    x = int(W * 0.11)
    # headline
    f_head = ImageFont.truetype(FONT_B, int(H * 0.115))
    d.text((x, int(H * 0.17)), "Your GPU is idle", font=f_head, fill=WHITE)
    d.text((x, int(H * 0.30)), "~95% of the day.", font=f_head, fill=CYAN)

    # sub
    f_sub = ImageFont.truetype(FONT_R, int(H * 0.055))
    d.text((x, int(H * 0.48)), "It could be mining when nothing needs it —", font=f_sub, fill=WHITE)
    d.text((x, int(H * 0.555)), "and serving AI the moment someone asks.", font=f_sub, fill=WHITE)

    # small print
    f_small = ImageFont.truetype(FONT_R, int(H * 0.042))
    d.text((x, int(H * 0.72)), "One card. One job at a time. Testnet is free.", font=f_small, fill=MUTED)

    # CTA pill
    f_cta = ImageFont.truetype(FONT_B, int(H * 0.05))
    label = "gpncoin.ai"
    tb = d.textbbox((0, 0), label, font=f_cta)
    pw, ph = tb[2] - tb[0] + int(W * 0.05), tb[3] - tb[1] + int(H * 0.045)
    px, py = x, int(H * 0.815)
    d.rounded_rectangle([px, py, px + pw, py + ph], radius=int(ph / 2), outline=CYAN, width=max(2, int(H * 0.005)))
    d.text((px + (pw - (tb[2] - tb[0])) / 2 - tb[0], py + (ph - (tb[3] - tb[1])) / 2 - tb[1]),
           label, font=f_cta, fill=CYAN)

    img.resize((w, h), Image.LANCZOS).save(os.path.join(OUT, path), quality=95)
    return os.path.join(OUT, path)


def square(path="x-idle-gpu-square.png"):
    return card(1080, 1080, path)


if __name__ == "__main__":
    print(card())
    print(square())
