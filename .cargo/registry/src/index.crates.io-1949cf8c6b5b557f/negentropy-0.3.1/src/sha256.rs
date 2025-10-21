// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use alloc::vec::Vec;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
const SHA256_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

pub fn hash<T>(input: T) -> [u8; 32]
where
    T: AsRef<[u8]>,
{
    let mut data: Vec<u8> = input.as_ref().to_vec();
    let original_len: usize = data.len();
    let bit_len: u64 = (original_len * 8) as u64;

    // Pre-processing: padding the message
    data.push(0x80); // Append a '1' bit
    while data.len() % 64 != 56 {
        data.push(0x00);
    }

    // Append the original bit length as a 64-bit big-endian integer
    let mut bit_len_bytes: [u8; 8] = [0; 8];
    for i in 0..8 {
        bit_len_bytes[7 - i] = ((bit_len >> (i * 8)) & 0xff) as u8;
    }
    data.extend_from_slice(&bit_len_bytes);

    let mut hash: [u32; 8] = SHA256_INIT;

    for chunk in data.chunks(64) {
        let mut w: [u32; 64] = [0u32; 64];
        for (i, chunk_byte) in chunk.iter().enumerate() {
            w[i / 4] |= u32::from(*chunk_byte) << (24 - (i % 4) * 8);
        }

        for i in 16..64 {
            let s0: u32 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1: u32 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a: u32 = hash[0];
        let mut b: u32 = hash[1];
        let mut c: u32 = hash[2];
        let mut d: u32 = hash[3];
        let mut e: u32 = hash[4];
        let mut f: u32 = hash[5];
        let mut g: u32 = hash[6];
        let mut h: u32 = hash[7];

        for i in 0..64 {
            let s1: u32 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch: u32 = (e & f) ^ ((!e) & g);
            let temp1: u32 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0: u32 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj: u32 = (a & b) ^ (a & c) ^ (b & c);
            let temp2: u32 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        hash[0] = hash[0].wrapping_add(a);
        hash[1] = hash[1].wrapping_add(b);
        hash[2] = hash[2].wrapping_add(c);
        hash[3] = hash[3].wrapping_add(d);
        hash[4] = hash[4].wrapping_add(e);
        hash[5] = hash[5].wrapping_add(f);
        hash[6] = hash[6].wrapping_add(g);
        hash[7] = hash[7].wrapping_add(h);
    }

    let mut result: [u8; 32] = [0u8; 32];
    for (i, &h) in hash.iter().enumerate() {
        result[i * 4] = (h >> 24) as u8;
        result[i * 4 + 1] = (h >> 16) as u8;
        result[i * 4 + 2] = (h >> 8) as u8;
        result[i * 4 + 3] = h as u8;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    const HASHES: [(&str, &str); 10] = [
        ("Bitcoin: A Peer-to-Peer Electronic Cash System", "efb5c6729d8ce3e03fd03aec340540b24a788454d45e717089b1e59243e16f43"),
        ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        ("Hello, world!", "315f5bdb76d078c43b8ac0064e4a0164612b1fce77c869345bfc94c75894edd3"),
        ("Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.", "1878f645a878a7497d497ff21095f3c4dbd582545b50f30f945130d3a220fe65"),
        ("💡🔒🌍", "c6191a7c1c774bcb9b43941723e54aa6e5d3c693994fadcf4068a161f8317978"),
        ("hello", "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
        ("Rust implementation of SHA-256", "883118983ae2f8764456714f3b33f62b8b9ad092021a2adff97adc208eee0948"),
        ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
        ("abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu", "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"),
    ];

    #[test]
    fn test_sha256() {
        for (data, output) in HASHES.iter() {
            let hash = hash(data);
            let hash = hex::encode(hash);
            assert_eq!(hash.as_bytes(), output.as_bytes());
        }
    }
}

#[cfg(bench)]
mod benches {
    use super::*;
    use crate::test::{black_box, Bencher};

    #[bench]
    pub fn sha256_hash(bh: &mut Bencher) {
        bh.iter(|| {
            black_box(sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        });
    }
}
