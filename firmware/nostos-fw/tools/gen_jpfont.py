# 日本語グリフ（16×16・1bpp）を Windows フォントから生成し src/jpfont.rs を書き出す。
#   python tools/gen_jpfont.py
# 文字を増やしたら CHARS に追記して再実行（生成物はコミットする）。
# フォント: BIZ UDゴシック（Windows 標準・視認性重視）。
from PIL import Image, ImageDraw, ImageFont
from pathlib import Path

CHARS = "軌跡帰路設定明るさ自動消灯凡例受信途絶低電池充電中"
SIZE = 16
FONT_PATH = r"C:\Windows\Fonts\BIZ-UDGothicR.ttc"

out = Path(__file__).resolve().parent.parent / "src" / "jpfont.rs"
font = ImageFont.truetype(FONT_PATH, SIZE)

lines = [
    "//! 日本語グリフ（16×16・1bpp・行ごと 2 バイト MSB ファースト）。",
    "//! `tools/gen_jpfont.py` による生成物。手編集しない（文字追加は CHARS に追記して再生成）。",
    "//! フォント: BIZ UDゴシック（Windows 同梱・(C) Morisawa Inc. / SIL OFL 系配布の UD ゴシック）。",
    "",
    "/// 1 グリフ = 16 行 × 2 バイト（MSB が左端画素）。",
    "pub const GLYPH_W: i32 = 16;",
    "/// グリフの高さ [px]。",
    "pub const GLYPH_H: i32 = 16;",
    "",
    "/// 収録グリフ（文字コード昇順）。",
    "pub const GLYPHS: &[(char, [u8; 32])] = &[",
]

seen = {}
for ch in CHARS:
    if ch in seen:
        continue
    img = Image.new("L", (SIZE, SIZE), 0)
    d = ImageDraw.Draw(img)
    # anchor="lt" でセル左上基準。UD ゴシックは全角がほぼセルいっぱいに乗る。
    d.text((0, 0), ch, fill=255, font=font, anchor="lt")
    rows = []
    for y in range(SIZE):
        v = 0
        for x in range(SIZE):
            if img.getpixel((x, y)) >= 128:
                v |= 1 << (15 - x)
        rows.append(v)
    seen[ch] = rows

for ch in sorted(seen, key=ord):
    rows = seen[ch]
    b = []
    for v in rows:
        b.append(f"0x{(v >> 8) & 0xFF:02X}")
        b.append(f"0x{v & 0xFF:02X}")
    lines.append(f"    ('{ch}', [{', '.join(b)}]),")

lines += [
    "];",
    "",
    "/// 文字のグリフを返す（未収録は None）。",
    "pub fn glyph(c: char) -> Option<&'static [u8; 32]> {",
    "    GLYPHS",
    "        .binary_search_by_key(&c, |(ch, _)| *ch)",
    "        .ok()",
    "        .map(|i| &GLYPHS[i].1)",
    "}",
    "",
]

out.write_text("\n".join(lines), encoding="utf-8")
print(f"wrote {out} ({len(seen)} glyphs)")
