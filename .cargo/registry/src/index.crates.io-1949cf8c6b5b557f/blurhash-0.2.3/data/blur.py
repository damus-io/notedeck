# Blur the test images using the official blurhash-python package.
# The results are used as reference images in the tests of blurhash-rs.

import blurhash
from os import listdir
from PIL import Image

images = [f for f in listdir(".") if "png" in f and not "blurred" in f]

for path in images:
    with Image.open(path) as image:
        hash = blurhash.encode(image, x_components=4, y_components=3)
        width, height = image.size

    result = blurhash.decode(hash, width, height, mode=blurhash.PixelMode.RGBA)
    result.save(path.split(".")[0] + "_blurred.png")
