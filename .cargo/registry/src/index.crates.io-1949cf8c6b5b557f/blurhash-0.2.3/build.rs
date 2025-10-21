use std::io::Write;

const LINEAR_TO_SRGB_LOOKUP_SIZE: usize = 8192;

fn linear_to_srgb(value: f32) -> u8 {
    let v = value.clamp(0., 1.);
    if v <= 0.003_130_8 {
        (v * 12.92 * 255. + 0.5).round() as u8
    } else {
        ((1.055 * 255.) * f32::powf(v, 1. / 2.4) - (0.055 * 255. - 0.5)).round() as u8
    }
}

fn generate_linear_to_srgb_lookup() -> [u8; LINEAR_TO_SRGB_LOOKUP_SIZE] {
    let mut table = [0u8; LINEAR_TO_SRGB_LOOKUP_SIZE];
    for i in 0..table.len() {
        let float = i as f32 / (table.len() - 1) as f32;
        table[i] = linear_to_srgb(float);
    }
    table
}

/// srgb 0-255 integer to linear 0.0-1.0 floating point conversion.
pub fn srgb_to_linear(value: u8) -> f32 {
    let v = value as f32 / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        f32::powf((v + 0.055) / 1.055, 2.4)
    }
}

fn generate_srgb_lookup() -> [f32; 256] {
    let mut table = [0f32; 256];
    for (i, val) in table.iter_mut().enumerate() {
        *val = srgb_to_linear(i as u8);
    }
    table
}

fn write_srgb(f: &mut std::fs::File) {
    writeln!(
        f,
        "
        static SRGB_LOOKUP: [f32; 256] = {:?};
        #[cfg(feature = \"fast-linear-to-srgb\")]
        const LINEAR_TO_SRGB_LOOKUP_SIZE: usize = {};
        #[cfg(feature = \"fast-linear-to-srgb\")]
        static LINEAR_TO_SRGB_LOOKUP: [u8; LINEAR_TO_SRGB_LOOKUP_SIZE] = {:?};
        ",
        generate_srgb_lookup(),
        LINEAR_TO_SRGB_LOOKUP_SIZE,
        generate_linear_to_srgb_lookup()
    )
    .unwrap();
}

fn write_base83(f: &mut std::fs::File) {
    const CHARACTERS: &[u8; 83] =
        b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";
    writeln!(f, "const CHARACTERS: [u8; 83] = {:?};", CHARACTERS).unwrap();

    let max_plus_one = CHARACTERS.iter().max().unwrap() + 1;
    let mut inv_map: [u8; 256] = [max_plus_one; 256];
    for (i, &c) in CHARACTERS.iter().enumerate() {
        inv_map[c as usize] = i as u8;
    }
    writeln!(
        f,
        "const CHARACTERS_INV: [u8; {max_plus_one}] = {:?};",
        &inv_map[0..max_plus_one as usize]
    )
    .unwrap();
    writeln!(f, "const CHARACTERS_INV_INVALID: u8 = {};", max_plus_one).unwrap();
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_dir = std::path::PathBuf::from(out_dir);

    let mut f = std::fs::File::create(out_dir.join("srgb_lookup.rs")).unwrap();
    write_srgb(&mut f);

    let mut f = std::fs::File::create(out_dir.join("base83_lookup.rs")).unwrap();
    write_base83(&mut f);
}
