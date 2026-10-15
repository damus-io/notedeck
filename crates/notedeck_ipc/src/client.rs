//! The calling end: a small blocking client for `nd` and `notedeck-spawn`.

use crate::{Request, Response};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// One connection to the running notedeck, good for one request.
pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connect to the notedeck listening on `path` (usually
    /// [`socket_path`](crate::socket_path)).
    pub fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Send `request` as one line.
    pub fn send(&mut self, request: &Request) -> io::Result<()> {
        self.send_line(&request.to_line())
    }

    /// Send a raw line, for a request this crate can't spell.
    pub fn send_line(&mut self, line: &str) -> io::Result<()> {
        let stream = self.reader.get_mut();
        writeln!(stream, "{line}")?;
        stream.flush()
    }

    /// Read the one answer line, without its newline. Blocks until notedeck
    /// answers or hangs up; a hang-up with no answer is an error.
    pub fn recv_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "notedeck hung up without answering",
            ));
        }
        Ok(line.trim_end().to_owned())
    }

    /// [`send`](Self::send) `request` and read its [`Response`].
    pub fn request(&mut self, request: &Request) -> io::Result<Response> {
        self.send(request)?;
        let line = self.recv_line()?;
        Response::from_line(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}
