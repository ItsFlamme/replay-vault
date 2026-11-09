"""The app icon: the Replay Vault cassette on its dark tile, as on the Discord server.
Writes assets/replay-vault.ico (16 to 256 px, embedded in the exe by build.rs) and
assets/replay-vault-256.png (the window and taskbar icon).
Usage: py -I tools/app_icon.py <repo>"""
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(sys.argv[1])
VT = ROOT / "assets/fonts/VT323-Regular.ttf"
PURPLE, PINK, ORANGE = (132, 72, 240), (230, 80, 170), (240, 120, 60)
BG = (17, 11, 26)
S = 512


def font(px):
    return ImageFont.truetype(str(VT), px)


def reels(img, win, w, ss=4):
    wx0, wy0, wx1, wy1 = win
    hh, cy = wy1 - wy0, (wy0 + wy1) / 2
    pad = max(2, int(w * 0.006)) + 1
    box = (int(wx0) + pad, int(wy0) + pad, int(wx1) - pad, int(wy1) - pad)
    patch = Image.new("RGBA", ((box[2] - box[0]) * ss, (box[3] - box[1]) * ss), (12, 8, 18, 255))
    d = ImageDraw.Draw(patch)
    big, small = hh * 0.42, hh * 0.3
    for fx, r in [(0.25, big), (0.75, small)]:
        cx, c = (wx0 + (wx1 - wx0) * fx - box[0]) * ss, (cy - box[1]) * ss
        d.ellipse((cx - r * ss, c - r * ss, cx + r * ss, c + r * ss), fill=(60, 40, 30, 255))
        hub = big * 0.45 * ss
        d.ellipse((cx - hub, c - hub, cx + hub, c + hub), fill=(225, 220, 235, 255))
        for k in range(6):
            a = k * math.pi / 3
            d.line((cx, c, cx + math.cos(a) * hub * 0.9, c + math.sin(a) * hub * 0.9), fill=(120, 110, 140, 255), width=max(1, int(w * 0.006)) * ss)
    img.alpha_composite(patch.resize((box[2] - box[0], box[3] - box[1]), Image.LANCZOS), box[:2])


def cassette(img, x, y, w):
    h = int(w * 0.62)
    d = ImageDraw.Draw(img)
    shadow = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((x, y + w * 0.04, x + w, y + h + w * 0.04), w * 0.06, fill=(0, 0, 0, 170))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(w * 0.04)))
    d.rounded_rectangle((x, y, x + w, y + h), w * 0.06, fill=(34, 24, 48, 255), outline=(70, 52, 96, 255), width=max(2, int(w * 0.008)))
    lx0, ly0, lx1, ly1 = x + w * 0.07, y + h * 0.1, x + w * 0.93, y + h * 0.5
    d.rounded_rectangle((lx0, ly0, lx1, ly1), w * 0.02, fill=(238, 230, 214, 255))
    sh = (ly1 - ly0) / 8
    for i, c in enumerate([PURPLE, PINK, ORANGE]):
        d.rectangle((lx0, ly1 - sh * (3 - i), lx1, ly1 - sh * (2 - i)), fill=c + (255,))
    d.text((lx0 + w * 0.04, ly0 + (ly1 - ly0) * 0.32), "REPLAY VAULT", font=font(int(w * 0.075)), fill=(40, 28, 56, 255), anchor="lm")
    d.text((lx1 - w * 0.04, ly0 + (ly1 - ly0) * 0.32), "T-120", font=font(int(w * 0.055)), fill=(110, 90, 130, 255), anchor="rm")
    win = (x + w * 0.22, y + h * 0.58, x + w * 0.78, y + h * 0.9)
    d.rounded_rectangle(win, w * 0.03, fill=(12, 8, 18, 255), outline=(80, 60, 110, 255), width=max(2, int(w * 0.006)))
    reels(img, win, w)


img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(img).rounded_rectangle((8, 8, S - 8, S - 8), 96, fill=BG + (255,), outline=PURPLE + (255,), width=18)
cassette(img, 56, 110, 400)
out = ROOT / "assets"
img.resize((256, 256), Image.LANCZOS).save(out / "replay-vault-256.png", optimize=True)
img.save(out / "replay-vault.ico", sizes=[(n, n) for n in (16, 20, 24, 32, 40, 48, 64, 128, 256)])
print("icon ok")
