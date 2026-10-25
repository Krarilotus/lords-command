"""Regenerate the checked-in app icons (development only; requires Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

assets = Path(__file__).resolve().parent.parent / 'assets'
assets.mkdir(exist_ok=True)
image = Image.new('RGBA', (256, 256), (0, 0, 0, 0))
draw = ImageDraw.Draw(image)
draw.rounded_rectangle((10, 10, 246, 246), radius=34, fill=(4, 9, 6, 255),
                       outline=(57, 255, 136, 255), width=12)
# Layers by narrowness: global (widest, bottom), project, repo (narrowest, top), each in its own hue.
for row, (inset, color) in enumerate([(92, (230, 200, 140)), (66, (0, 210, 190)), (40, (190, 140, 255))]):
    top = 62 + row * 48
    draw.rounded_rectangle((inset, top, 256 - inset, top + 30), radius=6, fill=(*color, 255))
image.save(assets / 'icon.ico', sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
image.resize((64, 64), Image.Resampling.LANCZOS).save(assets / 'icon.png')
