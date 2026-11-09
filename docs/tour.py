"""Builds docs/tour.gif from the app's own renders (the `screens` test): each scene shows a page on
the left and a caption on the right, in the website's VHS look (VT323 headings, purple glow,
scanlines, a REC counter that keeps running).

Usage: py -I docs/tour.py <renders dir> <vt323.ttf> <atkinson.ttf> <out.gif>
"""
import random
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

SHOTS, VT, BODY, OUT = sys.argv[1:5]

# (render, keys, heading, line). Keys are the app's defaults.
SCENES = [
    ("clips.png", "Alt+F10", "Press a key, keep the play", "The last minute of your game is saved as a clip, sorted by game."),
    ("clips-scrub.png", "", "Scrub without opening", "Hover a clip to scrub through it, or play it in the built-in player."),
    (["waveform.png", "trim-parts.png"], "", "Trim the boring part", "A waveform marks the loud moments. Keep one part or join several."),
    ("clips-export.png", "", "Sized to send", "A copy that fits Discord, 720p or 9:16 for Shorts, or a share link."),
    ("clips-tags.png", "", "Stars and tags", "Search, star and tag clips. Starred ones are never cleaned up."),
    ("audio.png", "", "Game and mic apart", "Each sound on its own track, so you can drop your mic later."),
    ("setup2.png", "", "Set up in a minute", "Quality is picked for your PC. Your own OBS is never touched."),
    ("keybinds-nvidia.png", "", "Keys that stay yours", "Change any key. If NVIDIA or AMD already has one, it says which."),
]

W, H = 1200, 600
PANEL = 340
APP_W = W - PANEL - 48
APP_H = round(APP_W * 800 / 1280)
FPS, HOLD, FADE = 12, 4.0, 0.5
BG, PURPLE, LILAC, RED, WHITE, MUTED = (17, 11, 26), (132, 72, 240), (176, 140, 255), (255, 72, 80), (246, 242, 255), (179, 170, 196)


def vt(px):
    return ImageFont.truetype(VT, px)


def body(px):
    return ImageFont.truetype(BODY, px)


def glow_text(img, xy, text, f, fill=WHITE, glow=PURPLE, radius=8, anchor="la"):
    layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).text(xy, text, font=f, fill=glow + (255,), anchor=anchor)
    img.alpha_composite(layer.filter(ImageFilter.GaussianBlur(radius)))
    ImageDraw.Draw(img).text(xy, text, font=f, fill=fill + (255,), anchor=anchor)


def wrap(text, f, width):
    lines, line = [], ""
    for word in text.split():
        test = f"{line} {word}".strip()
        if f.getlength(test) > width and line:
            lines.append(line)
            line = word
        else:
            line = test
    return lines + [line]


def backdrop():
    img = Image.new("RGBA", (W, H), BG + (255,))
    blob = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(blob).ellipse((-200, 100, 900, 900), fill=PURPLE + (70,))
    img.alpha_composite(blob.filter(ImageFilter.GaussianBlur(160)))
    ImageDraw.Draw(img).line((W - PANEL, 0, W - PANEL, H), fill=(53, 42, 71, 255), width=1)
    return img


def scanlines():
    lines = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(lines)
    for y in range(0, H, 4):
        d.line((0, y, W, y), fill=(0, 0, 0, 34))
    random.seed(3)
    p = lines.load()
    for _ in range(W * H // 30):
        v = random.randrange(190, 256)
        p[random.randrange(W), random.randrange(H)] = (v, v, v, random.randrange(4, 12))
    return lines


def keycaps(img, x, y, keys):
    d = ImageDraw.Draw(img)
    f = vt(30)
    for i, k in enumerate(keys.split("+")):
        if i:
            d.text((x + 4, y + 17), "+", font=f, fill=MUTED + (255,), anchor="lm")
            x += 22
        w = int(f.getlength(k)) + 24
        d.rounded_rectangle((x, y + 3, x + w, y + 37), 7, fill=(12, 8, 18, 255))
        d.rounded_rectangle((x, y, x + w, y + 32), 7, fill=(36, 27, 48, 255), outline=(86, 66, 120, 255))
        d.text((x + w / 2, y + 16), k, font=f, fill=WHITE + (255,), anchor="mm")
        x += w + 6


def caption(img, keys, heading, line):
    x, width = W - PANEL + 32, PANEL - 64
    hf, bf = vt(46), body(18)
    head = wrap(heading, hf, width)
    text = wrap(line, bf, width)
    block = (52 if keys else 0) + len(head) * 44 + 14 + len(text) * 26
    y = (H - block) // 2
    if keys:
        keycaps(img, x, y, keys)
        y += 52
    for h in head:
        glow_text(img, (x, y), h, hf)
        y += 44
    y += 14
    for t in text:
        ImageDraw.Draw(img).text((x, y), t, font=bf, fill=MUTED + (255,))
        y += 26
    ImageDraw.Draw(img).text((x, H - 40), "replayvault.app", font=vt(24), fill=LILAC + (255,))


def page_of(shot):
    """A full page render, or strips (like the trim bar) stacked on the app's background."""
    if isinstance(shot, str):
        return Image.open(f"{SHOTS}/{shot}").convert("RGBA")
    page = Image.new("RGBA", (1280, 800), (20, 20, 20, 255))
    strips = [Image.open(f"{SHOTS}/{s}").convert("RGBA") for s in shot]
    y = (800 - sum(s.height for s in strips) - 40 * (len(strips) - 1)) // 2
    for s in strips:
        page.alpha_composite(s, ((1280 - s.width) // 2, y))
        y += s.height + 40
    return page


def app_window(img, shot):
    page = page_of(shot).resize((APP_W, APP_H), Image.LANCZOS)
    x, y = 24, (H - APP_H) // 2
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((x, y + 8, x + APP_W, y + APP_H + 8), 12, fill=(0, 0, 0, 160))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(14)))
    mask = Image.new("L", (APP_W, APP_H), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, APP_W - 1, APP_H - 1), 10, fill=255)
    img.paste(page, (x, y), mask)
    ImageDraw.Draw(img).rounded_rectangle((x, y, x + APP_W - 1, y + APP_H - 1), 10, outline=(70, 56, 92, 255))


def osd(img, t):
    d = ImageDraw.Draw(img)
    if int(t * 2) % 2 == 0:
        d.ellipse((W - PANEL + 32, 26, W - PANEL + 46, 40), fill=RED + (255,))
    glow_text(img, (W - PANEL + 54, 33), "REC", vt(30), glow=RED, radius=5, anchor="lm")
    s = int(t)
    glow_text(img, (W - 32, 33), f"0:{s // 60:02}:{s % 60:02}", vt(30), radius=5, anchor="rm")


lines = scanlines()
stills = []
for shot, keys, heading, line in SCENES:
    s = backdrop()
    app_window(s, shot)
    caption(s, keys, heading, line)
    stills.append(s)

frames, t = [], 0.0
for i, s in enumerate(stills):
    nxt = stills[(i + 1) % len(stills)]
    for k in range(int((HOLD + FADE) * FPS)):
        a = max(0.0, (k / FPS - HOLD) / FADE)
        f = Image.blend(s, nxt, a) if a > 0 else s.copy()
        osd(f, t)
        f.alpha_composite(lines)
        frames.append(f.convert("RGB"))
        t += 1 / FPS

# One shared palette keeps the file small and the colors steady between frames.
pal = frames[0].quantize(255, method=Image.Quantize.MEDIANCUT)
gif = [f.quantize(palette=pal, dither=Image.Dither.NONE) for f in frames]
gif[0].save(OUT, save_all=True, append_images=gif[1:], duration=1000 // FPS, loop=0, optimize=True)
print(OUT, len(gif), "frames")
