from io import BytesIO
from pathlib import Path

import cairosvg
from PIL import Image


root = Path(__file__).resolve().parents[1]
svg = (root / "assets" / "openmixer.svg").read_bytes()
images = []
for size in (16, 24, 32, 48, 64, 128, 256):
    png = cairosvg.svg2png(bytestring=svg, output_width=size, output_height=size)
    images.append(Image.open(BytesIO(png)).convert("RGBA"))

images[-1].save(root / "assets" / "openmixer.png", format="PNG")
images[-1].save(
    root / "assets" / "openmixer.ico",
    format="ICO",
    append_images=images[:-1],
    sizes=[(image.width, image.height) for image in images],
)
