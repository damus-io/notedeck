use bitcoin::hashes::hmac::{Hmac, HmacEngine};
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::{Hash, HashEngine};

macro_rules! hkdf_extract_expand {
    ($salt: expr, $ikm: expr) => {{
        let mut hmac = HmacEngine::<Sha256>::new($salt);
        hmac.input($ikm);
        let prk = Hmac::from_engine(hmac).to_byte_array();
        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&[1; 1]);
        let t1 = Hmac::from_engine(hmac).to_byte_array();
        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&t1);
        hmac.input(&[2; 1]);
        (t1, Hmac::from_engine(hmac).to_byte_array(), prk)
    }};
    ($salt: expr, $ikm: expr, 2) => {{
        let (k1, k2, _) = hkdf_extract_expand!($salt, $ikm);
        (k1, k2)
    }};
    ($salt: expr, $ikm: expr, 6) => {{
        let (k1, k2, prk) = hkdf_extract_expand!($salt, $ikm);

        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&k2);
        hmac.input(&[3; 1]);
        let k3 = Hmac::from_engine(hmac).to_byte_array();

        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&k3);
        hmac.input(&[4; 1]);
        let k4 = Hmac::from_engine(hmac).to_byte_array();

        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&k4);
        hmac.input(&[5; 1]);
        let k5 = Hmac::from_engine(hmac).to_byte_array();

        let mut hmac = HmacEngine::<Sha256>::new(&prk[..]);
        hmac.input(&k5);
        hmac.input(&[6; 1]);
        let k6 = Hmac::from_engine(hmac).to_byte_array();

        (k1, k2, k3, k4, k5, k6)
    }};
}

pub fn hkdf_extract_expand_twice(salt: &[u8], ikm: &[u8]) -> ([u8; 32], [u8; 32]) {
    hkdf_extract_expand!(salt, ikm, 2)
}
