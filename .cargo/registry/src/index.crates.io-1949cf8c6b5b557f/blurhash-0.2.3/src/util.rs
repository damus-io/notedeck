include!(concat!(env!("OUT_DIR"), "/srgb_lookup.rs"));

/// linear 0.0-1.0 floating point to srgb 0-255 integer conversion.
#[cfg(not(feature = "fast-linear-to-srgb"))]
pub fn linear_to_srgb(value: f32) -> u8 {
    let v = value.clamp(0., 1.);
    if v <= 0.003_130_8 {
        (v * 12.92 * 255. + 0.5).round() as u8
    } else {
        // The original C implementation uses this formula:
        // ((1.055 * f32::powf(v, 1. / 2.4) - 0.055) * 255. + 0.5).round() as u8
        // But we can distribute the latter multiplication, to reduce the number of operations:
        ((1.055 * 255.) * f32::powf(v, 1. / 2.4) - (0.055 * 255. - 0.5)).round() as u8
    }
}

/// linear 0.0-1.0 floating point to srgb 0-255 integer conversion.
#[cfg(feature = "fast-linear-to-srgb")]
pub fn linear_to_srgb(value: f32) -> u8 {
    let v = value.clamp(0.0, 1.0);
    let index =
        ((LINEAR_TO_SRGB_LOOKUP_SIZE as f32 * v) as usize).min(LINEAR_TO_SRGB_LOOKUP_SIZE - 1);
    LINEAR_TO_SRGB_LOOKUP[index]
}

/// srgb 0-255 integer to linear 0.0-1.0 floating point conversion.
pub fn srgb_to_linear(value: u8) -> f32 {
    SRGB_LOOKUP[value as usize]
}

pub fn sign_pow(val: f32, exp: f32) -> f32 {
    f32::copysign(f32::powf(val.abs(), exp), val)
}
