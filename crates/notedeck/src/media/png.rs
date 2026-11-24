//! PNG-encoding pixels that arrive as an [`egui::ColorImage`], such as a
//! screenshot pasted from the clipboard ([`egui::Event::PasteImage`]).

use egui::ColorImage;

/// `image` as PNG bytes, in straight (unmultiplied) sRGBA.
///
/// Encoding a screenshot takes a while, so call this off the render loop
/// (on the [`crate::JobPool`]).
pub fn encode_color_image(image: &ColorImage) -> Result<Vec<u8>, String> {
    let [w, h] = image.size;
    let rgba: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|c| c.to_srgba_unmultiplied())
        .collect();
    let png = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or("The pasted image has the wrong size")?;
    let mut bytes = Vec::new();
    png.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("The pasted image could not be encoded: {e}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes decode back to the same size and pixels.
    #[test]
    fn round_trips_an_opaque_image() {
        let image = ColorImage::new([3, 2], vec![egui::Color32::from_rgb(10, 200, 30); 6]);
        let png = encode_color_image(&image).unwrap();
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(2, 1).0, [10, 200, 30, 255]);
    }
}
