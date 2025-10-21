use super::util::sign_pow;

pub fn encode(value: [f32; 3], maximum_value: f32) -> u32 {
    let quant_r = i32::max(
        0,
        i32::min(
            18,
            f32::floor(sign_pow(value[0] / maximum_value, 0.5) * 9. + 9.5) as i32,
        ),
    );
    let quant_g = i32::max(
        0,
        i32::min(
            18,
            f32::floor(sign_pow(value[1] / maximum_value, 0.5) * 9. + 9.5) as i32,
        ),
    );
    let quant_b = i32::max(
        0,
        i32::min(
            18,
            f32::floor(sign_pow(value[2] / maximum_value, 0.5) * 9. + 9.5) as i32,
        ),
    );

    (quant_r * 19 * 19 + quant_g * 19 + quant_b) as u32
}

pub fn decode(value: u32, maximum_value: f32) -> [f32; 3] {
    let quant_r = f32::floor(value as f32 / (19. * 19.));
    let quant_g = f32::floor(value as f32 / 19.) % 19.;
    let quant_b = value as f32 % 19.;

    [
        sign_pow((quant_r - 9.) / 9., 2.0) * maximum_value,
        sign_pow((quant_g - 9.) / 9., 2.0) * maximum_value,
        sign_pow((quant_b - 9.) / 9., 2.0) * maximum_value,
    ]
}
