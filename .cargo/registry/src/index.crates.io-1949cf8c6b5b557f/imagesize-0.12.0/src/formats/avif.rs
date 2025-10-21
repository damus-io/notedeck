// AVIF is a special case of HEIF. Image size methods are defined in there and should work for both.
// Only difference is that we want to specify the image type as AVIF instead of wrapping it into HEIF.
pub fn matches(header: &[u8]) -> bool {
    if header.len() < 12 || &header[4..8] != b"ftyp" {
        return false;
    }

    let header_brand = &header[8..12];

    // Since other non-AVIF files may contain ftype in the header
    // we try to use brands to distinguish image files specifically.
    // List of brands from here: https://mp4ra.org/#/brands
    let valid_brands = [
        b"avif", b"avio", b"avis", b"MA1A",
        b"MA1B",
    ];

    for brand in valid_brands {
        if brand == header_brand {
            return true;
        }
    }
    
    false
}
