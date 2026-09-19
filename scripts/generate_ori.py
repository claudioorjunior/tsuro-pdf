#!/usr/bin/env python3
"""Ori — gerador dos 21 icones da toolbar Tsuro.

Data-driven: ICONS mapeia nome -> lista de elementos; o render resolve as
cores via HI/MID/DARK (nunca hex literal nos dados). Audita paleta e
stroke-width, escreve os SVGs, e renderiza sheets old/novo em /tmp/ori-build.

Uso: python3 scripts/generate_ori.py   (a partir da raiz do repo)
"""
import os
import re
import shutil
import subprocess
import sys

HI = "#5ec9c1"
MID = "#3a9d95"
DARK = "#247870"
PAL = {"HI": HI, "MID": MID, "DARK": DARK}

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORI = os.path.join(ROOT, "crates", "tsuro", "assets", "icons", "ori")
OUT = "/tmp/ori-build"

HDR = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"'
    ' viewBox="0 0 24 24" fill="none" aria-hidden="true">\n'
)

# Elementos: ("path", cor, d) | ("stroke", cor, w, d, cap, join)
#            ("rect", cor, x, y, w, h, rx|None) | ("circle", cor, cx, cy, r)
ICONS = {
    "search": [
        ("stroke", "HI", 3, "M6.46 13.54A5 5 0 0 1 13.54 6.46", None, None),
        ("stroke", "MID", 3, "M13.54 6.46A5 5 0 0 1 6.46 13.54", None, None),
        ("stroke", "MID", 3.4, "M13.6 13.6L19.4 19.4", "round", None),
    ],
    "folder": [
        ("path", "HI", "M4 10V6H9L11 4H20V10Z"),
        ("path", "MID", "M3 10H21L13 20H3Z"),
        ("path", "HI", "M21 10V20H13Z"),
        ("path", "DARK", "M3 18.4H21V20H3Z"),
    ],
    "strike": [
        ("rect", "MID", 5, 5, 14, 2, 1),
        ("rect", "MID", 5, 11, 10, 2, 1),
        ("rect", "MID", 5, 17, 14, 2, 1),
        ("path", "HI", "M3.5 10.4H20.5V12H3.5Z"),
        ("path", "DARK", "M3.5 12H20.5V12.8H3.5Z"),
    ],
    "underline": [
        ("rect", "MID", 5, 5, 14, 2, 1),
        ("rect", "MID", 5, 10, 10, 2, 1),
        ("path", "HI", "M4 15.5H20V17.3H4Z"),
        ("path", "DARK", "M4 17.3H20V18.2H4Z"),
    ],
    "chevron-left": [
        ("stroke", "MID", 3.6, "M16 6L9 12L16 18", "round", "round"),
        ("stroke", "HI", 3.6, "M16 6L9 12", "butt", None),
    ],
    "chevron-right": [
        ("stroke", "MID", 3.6, "M8 6L15 12L8 18", "round", "round"),
        ("stroke", "HI", 3.6, "M8 6L15 12", "butt", None),
    ],
    "minus": [
        ("path", "MID", "M5 10.7H19V13.3H5Z"),
        ("path", "HI", "M5 10.7H12V13.3H5Z"),
        ("path", "DARK", "M16 10.7H19V13.3H16Z"),
    ],
    "plus": [
        ("path", "MID", "M10.8 4.8H13.2V10.8H19.2V13.2H13.2V19.2H10.8V13.2H4.8V10.8H10.8Z"),
        ("path", "HI", "M10.8 4.8H13.2V10.8H10.8Z"),
        ("path", "DARK", "M10.8 17.6H13.2V19.2H10.8Z"),
    ],
    "more": [
        ("circle", "MID", 5.3, 12, 2.3),
        ("circle", "HI", 12, 12, 2.3),
        ("circle", "MID", 18.7, 12, 2.3),
    ],
    "copy": [
        ("path", "HI", "M10 3H20.5V13.5H10Z"),
        ("path", "MID", "M3.5 10.5H14V21H3.5Z"),
        ("path", "DARK", "M3.5 19.4H14V21H3.5Z"),
    ],
    "file-text": [
        ("path", "MID", "M6.5 3H13.5L17.5 7V21H6.5Z"),
        ("path", "HI", "M13.5 3L17.5 7H13.5Z"),
        ("rect", "HI", 9, 10.5, 6, 1.8, None),
        ("rect", "HI", 9, 13.8, 6, 1.8, None),
        ("rect", "HI", 9, 17.1, 4, 1.8, None),
    ],
    "note": [
        ("path", "MID", "M6 3H18V15L14 19H6Z"),
        ("path", "HI", "M18 15L14 19H18Z"),
        ("path", "DARK", "M6 17.4H14V19H6Z"),
        ("rect", "HI", 8.5, 7, 7, 1.8, None),
    ],
    "pages": [
        ("path", "HI", "M9 3.5H18.5V16.5H9Z"),
        ("path", "MID", "M5.5 7.5H15V20.5H5.5Z"),
        ("path", "HI", "M12.5 7.5L15 10H12.5Z"),
        ("path", "DARK", "M5.5 18.9H15V20.5H5.5Z"),
    ],
    "folder-open": [
        ("path", "HI", "M4 9V5H9L11 3H20V9Z"),
        ("path", "DARK", "M4 9H20V11H4Z"),
        ("path", "MID", "M3 11H21L19 20H5Z"),
        ("path", "HI", "M3 11L5 20H8.5Z"),
        ("path", "DARK", "M5 18.4H19V20H5Z"),
    ],
    "fit-page": [
        ("path", "MID", "M8 4H16V17H8Z"),
        ("path", "HI", "M13.5 4L16 6.5H13.5Z"),
        ("path", "DARK", "M8 15.4H16V17H8Z"),
        ("stroke", "MID", 2, "M3.5 7V3.5H7", None, None),
        ("stroke", "MID", 2, "M17 3.5H20.5V7", None, None),
        ("stroke", "MID", 2, "M3.5 17V20.5H7", None, None),
        ("stroke", "MID", 2, "M17 20.5H20.5V17", None, None),
    ],
    "fit-width": [
        ("path", "MID", "M7 11H17V13H7Z"),
        ("path", "DARK", "M7 12H17V13H7Z"),
        ("path", "HI", "M7 9.3V14.7L3.5 12Z"),
        ("path", "HI", "M17 9.3V14.7L20.5 12Z"),
    ],
    "highlighter": [
        ("path", "MID", "M5.44 15.44L15.44 5.44L18.56 8.56L8.56 18.56Z"),
        ("path", "HI", "M5.44 15.44L8.56 18.56L4.53 19.47Z"),
        ("path", "DARK", "M14.03 6.85L17.15 9.97L16.16 10.96L13.04 7.84Z"),
        ("path", "HI", "M15.44 5.44L18.56 8.56L17.15 9.97L14.03 6.85Z"),
    ],
    "home": [
        ("path", "HI", "M2.5 12L12 4L21.5 12H19L12 6.5L5 12Z"),
        ("path", "MID", "M5.5 12H18.5V20H5.5Z"),
        ("path", "DARK", "M10.5 14.5H13.5V20H10.5Z"),
    ],
    "print": [
        ("path", "HI", "M7 3H17V9H7Z"),
        ("path", "MID", "M4 9H20V17H4Z"),
        ("path", "DARK", "M6 11H18V12.6H6Z"),
        ("path", "HI", "M7.5 14H16.5V20.5H7.5Z"),
    ],
    "shield": [
        ("path", "MID", "M12 2.5L5 5.5V11.5C5 16 8.2 19.2 12 21C15.8 19.2 19 16 19 11.5V5.5Z"),
        ("path", "HI", "M12 2.5L5 5.5V9H12Z"),
        ("stroke", "HI", 2.8, "M8.3 12L11.2 14.8L15.9 8.6", "round", "round"),
    ],
    "x": [
        ("path", "MID", "M7.2 5L19 16.8L16.8 19L5 7.2Z"),
        ("path", "HI", "M16.8 5L19 7.2L7.2 19L5 16.8Z"),
        ("path", "DARK", "M12 10.6L13.4 12L12 13.4L10.6 12Z"),
    ],
}


def el_svg(el) -> str:
    kind = el[0]
    if kind == "path":
        _, c, d = el
        return f'  <path fill="{PAL[c]}" d="{d}"/>\n'
    if kind == "stroke":
        _, c, w, d, cap, join = el
        extra = ""
        if cap:
            extra += f' stroke-linecap="{cap}"'
        if join:
            extra += f' stroke-linejoin="{join}"'
        return f'  <path fill="none" stroke="{PAL[c]}" stroke-width="{w}"{extra} d="{d}"/>\n'
    if kind == "rect":
        _, c, x, y, w, h, rx = el
        rxs = f' rx="{rx}"' if rx else ""
        return f'  <rect x="{x}" y="{y}" width="{w}" height="{h}"{rxs} fill="{PAL[c]}"/>\n'
    if kind == "circle":
        _, c, cx, cy, r = el
        return f'  <circle cx="{cx}" cy="{cy}" r="{r}" fill="{PAL[c]}"/>\n'
    raise ValueError(f"elemento desconhecido: {kind}")


def svg_text(name) -> str:
    body = "".join(el_svg(el) for el in ICONS[name])
    return HDR + f"  <!-- Ori \u00b7 {name} \u00b7 Tsuro \u00b7 gen -->\n" + body + "</svg>\n"


def audit(svgs) -> list:
    """Todo fill/stroke na paleta; todo stroke-width >= 1.5."""
    allowed = {HI, MID, DARK, "none"}
    errs = []
    for name, text in svgs.items():
        for v in re.findall(r'fill="([^"]+)"', text):
            if v not in allowed:
                errs.append(f"{name}: fill fora da paleta: {v}")
        for v in re.findall(r'stroke="([^"]+)"', text):
            if v not in allowed:
                errs.append(f"{name}: stroke fora da paleta: {v}")
        for v in re.findall(r'stroke-width="([^"]+)"', text):
            if float(v) < 1.5:
                errs.append(f"{name}: stroke-width < 1.5: {v}")
        for v in re.findall(r"#[0-9A-Za-z]+", text):
            if v.lower() not in allowed:
                errs.append(f"{name}: hex fora da paleta: {v}")
    return errs


def backup_old() -> None:
    old = os.path.join(OUT, "old")
    os.makedirs(old, exist_ok=True)
    for name in ICONS:
        dst = os.path.join(old, name + ".svg")
        if not os.path.exists(dst):
            shutil.copy(os.path.join(ORI, name + ".svg"), dst)


def write_new(svgs) -> None:
    new = os.path.join(OUT, "new")
    os.makedirs(new, exist_ok=True)
    for name, text in svgs.items():
        with open(os.path.join(ORI, name + ".svg"), "w") as f:
            f.write(text)
        with open(os.path.join(new, name + ".svg"), "w") as f:
            f.write(text)


def render_png() -> None:
    from PIL import Image, ImageDraw, ImageFont

    font = ImageFont.load_default()
    icons = list(ICONS)
    cols = ["old", "new"]
    pngs = {}
    for icon in icons:
        for c in cols:
            srcdir = os.path.join(OUT, c)
            src = os.path.join(srcdir, icon + ".svg")
            for size in (16, 32):
                dst = os.path.join(OUT, f"{icon}.{c}.{size}.png")
                subprocess.run(
                    ["rsvg-convert", "-w", str(size), "-h", str(size), src, "-o", dst],
                    check=True,
                )
                im = Image.open(dst).convert("RGBA")
                if im.getbbox() is None:
                    raise SystemExit(f"render vazio: {icon}.{c} @ {size}px")
                pngs[(icon, c, size)] = im

    for size, zoom in ((16, 3), (32, 2)):
        for theme, bg in (("dark", (26, 26, 26)), ("light", (247, 247, 246))):
            cell = size * zoom
            label_h = 12
            W = len(cols) * cell + 40
            H = len(icons) * (cell + label_h) + 40
            sheet = Image.new("RGB", (W, H), bg)
            d = ImageDraw.Draw(sheet)
            fg = (154, 152, 144) if theme == "dark" else (120, 118, 112)
            for r, icon in enumerate(icons):
                for i, c in enumerate(cols):
                    im = pngs[(icon, c, size)].resize(
                        (cell, cell), Image.NEAREST if size == 16 else Image.BILINEAR
                    )
                    x = 20 + i * cell
                    y = 20 + r * (cell + label_h)
                    sheet.paste(im, (x, y), im)
                    d.text((x + 4, y + cell), f"{icon}.{c}", font=font, fill=fg)
            sheet.save(os.path.join(OUT, f"sheet-{size}-{theme}.png"))
    print("png ok")


def build_html() -> None:
    cells = ""
    for icon in ICONS:
        for c in ("old", "new"):
            cells += (
                f'<div class="cell"><img src="{c}/{icon}.svg" alt="">'
                f"<span>{icon} \u00b7 {c}</span></div>\n"
            )
    html = (
        "<!doctype html><html lang='pt-BR'><meta charset='utf-8'>"
        "<title>Ori — old vs new</title><style>"
        ":root{--bg:#1a1a1a;--el:#262626;--ink:#ececea;--mut:#9a9890;--ln:#333331}"
        "html.light{--bg:#f7f7f6;--el:#fff;--ink:#1c1c1a;--mut:#787670;--ln:#e0ded9}"
        "body{font-family:system-ui;background:var(--bg);color:var(--ink);"
        "max-width:900px;margin:0 auto;padding:24px}"
        ".bar{display:flex;gap:8px;margin:12px 0}.bar button{border:1px solid var(--ln);"
        "background:var(--el);color:var(--ink);border-radius:8px;padding:6px 12px;cursor:pointer}"
        ".grid{display:grid;grid-template-columns:repeat(2,1fr);gap:12px}"
        ".cell{border:1px solid var(--ln);border-radius:12px;background:var(--el);"
        "padding:20px;text-align:center}.cell img{width:64px;height:64px}"
        ".cell span{font-size:12px;color:var(--mut);display:block;margin-top:8px}"
        ".row16 img{width:16px!important;height:16px!important}"
        "</style><h1>Ori — old vs new (21 \u00edcones)</h1>"
        "<div class='bar'><button onclick='t()'>tema</button>"
        "<button onclick='z()'>zoom 64/16</button></div>"
        f"<div class='grid' id='g'>{cells}</div>"
        "<script>function t(){document.documentElement.classList.toggle('light')}"
        "function z(){document.getElementById('g').classList.toggle('row16')}</script></html>"
    )
    with open(os.path.join(OUT, "sheet.html"), "w") as f:
        f.write(html)
    print("html ok")


def main() -> None:
    assert len(ICONS) == 21, f"esperado 21 icones, achado {len(ICONS)}"
    svgs = {name: svg_text(name) for name in ICONS}
    errs = audit(svgs)
    if errs:
        print("AUDIT FAIL:", file=sys.stderr)
        for e in errs:
            print("  " + e, file=sys.stderr)
        sys.exit(1)
    print("audit ok: 21 svg, paleta + stroke-width >= 1.5")
    backup_old()
    print("old ok: /tmp/ori-build/old/")
    write_new(svgs)
    print("svg ok: 21 escritos")
    render_png()
    build_html()


if __name__ == "__main__":
    main()
