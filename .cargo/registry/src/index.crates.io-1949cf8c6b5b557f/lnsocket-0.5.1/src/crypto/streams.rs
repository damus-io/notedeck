use crate::crypto::chacha20::ChaCha20;
use crate::crypto::chacha20poly1305rfc::ChaCha20Poly1305RFC;

use crate::util::ser::{Writeable, Writer};
use std::io::{self, Write};

pub struct ChaChaReader<'a, R: io::Read> {
    pub chacha: &'a mut ChaCha20,
    pub read: R,
}
impl<'a, R: io::Read> io::Read for ChaChaReader<'a, R> {
    fn read(&mut self, dest: &mut [u8]) -> Result<usize, io::Error> {
        let res = self.read.read(dest)?;
        if res > 0 {
            self.chacha.process_in_place(&mut dest[0..res]);
        }
        Ok(res)
    }
}

/// Enables the use of the serialization macros for objects that need to be simultaneously encrypted and
/// serialized. This allows us to avoid an intermediate Vec allocation.
pub struct ChaChaPolyWriteAdapter<'a, W: Writeable> {
    pub rho: [u8; 32],
    pub writeable: &'a W,
}

impl<'a, W: Writeable> ChaChaPolyWriteAdapter<'a, W> {
    #[allow(unused)] // This will be used for onion messages soon
    pub fn new(rho: [u8; 32], writeable: &'a W) -> ChaChaPolyWriteAdapter<'a, W> {
        Self { rho, writeable }
    }
}

impl<'a, T: Writeable> Writeable for ChaChaPolyWriteAdapter<'a, T> {
    // Simultaneously write and encrypt Self::writeable.
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        let mut chacha = ChaCha20Poly1305RFC::new(&self.rho, &[0; 12], &[]);
        let mut chacha_stream = ChaChaPolyWriter {
            chacha: &mut chacha,
            write: w,
        };
        self.writeable.write(&mut chacha_stream)?;
        let mut tag = [0_u8; 16];
        chacha.finish_and_get_tag(&mut tag);
        tag.write(w)?;

        Ok(())
    }
}

/// Enables simultaneously writing and encrypting a byte stream into a Writer.
struct ChaChaPolyWriter<'a, W: Writer> {
    pub chacha: &'a mut ChaCha20Poly1305RFC,
    pub write: &'a mut W,
}

impl<'a, W: Writer> Writer for ChaChaPolyWriter<'a, W> {
    // Encrypt then write bytes from `src` into Self::write.
    // `ChaCha20Poly1305RFC::finish_and_get_tag` can be called to retrieve the tag after all writes
    // complete.
    fn write_all(&mut self, src: &[u8]) -> Result<(), io::Error> {
        let mut src_idx = 0;
        while src_idx < src.len() {
            let mut write_buffer = [0; 8192];
            let bytes_written = (&mut write_buffer[..])
                .write(&src[src_idx..])
                .expect("In-memory writes can't fail");
            self.chacha
                .encrypt_in_place(&mut write_buffer[..bytes_written]);
            self.write.write_all(&write_buffer[..bytes_written])?;
            src_idx += bytes_written;
        }
        Ok(())
    }
}
