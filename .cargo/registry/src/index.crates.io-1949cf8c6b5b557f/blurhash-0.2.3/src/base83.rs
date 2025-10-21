use crate::Error;

include!(concat!(env!("OUT_DIR"), "/base83_lookup.rs"));

pub fn encode_into(value: u32, length: u32, s: &mut String) {
    for i in 1..=length {
        let digit: u32 = (value / u32::pow(83, length - i)) % 83;
        s.push(CHARACTERS[digit as usize] as char);
    }
}

pub fn decode(str: &str) -> Result<u64, Error> {
    // log_83(2^64) = 10.03
    if str.len() > 10 {
        panic!("base83::decode can only process strings up to 10 characters");
    }
    let mut value = 0;

    for byte in str.as_bytes() {
        if *byte as usize >= CHARACTERS_INV.len() {
            return Err(Error::InvalidBase83(*byte));
        }
        let digit = CHARACTERS_INV[*byte as usize];
        if digit == CHARACTERS_INV_INVALID {
            return Err(Error::InvalidBase83(*byte));
        }
        value = value * 83 + digit as u64;
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode_into};

    fn encode(value: u32, length: u32) -> String {
        let mut s = String::new();
        encode_into(value, length, &mut s);
        s
    }

    #[test]
    fn encode83() {
        let str = encode(6869, 2);
        assert_eq!(str, "~$");
    }

    #[test]
    fn decode83() {
        let v = decode("~$").unwrap();
        assert_eq!(v, 6869);
    }

    #[test]
    fn decode83_too_large() {
        assert!(decode("€").is_err());
    }

    #[test]
    #[should_panic]
    fn decode83_too_long() {
        let _ = decode("~$aaaaaaaaa");
    }
}
