// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: Apache-2.0

//! `tracing` writer for logging into Android's logcat. Instead of linking
//! liblog, which isn't available as a static library in the NDK, this library
//! directly connects to logd and sends messages via the [documented protocol].
//!
//! There are a few behavioral differences compared to liblog:
//!
//! * In the very unlikely event that Android's logd crashes, logging will stop
//!   working because tracing-logcat does not attempt to reconnect to the logd
//!   socket.
//! * Only Android 5 and newer are supported. Previous versions of Android did
//!   not use logd and implemented logcat without a userspace daemon.
//! * Log messages longer than `4068 - <tag length> - 2` bytes are split into
//!   multiple messages instead of being truncated. If the original message is
//!   valid UTF-8, then the message is split at a code point boundary (not at a
//!   grapheme cluster boundary). Otherwise, the message is split exactly at the
//!   length limit.
//!
//! [documented protocol]: https://cs.android.com/android/platform/superproject/main/+/main:system/logging/liblog/README.protocol.md
//!
//! ## Examples
//!
//! ### Using a fixed tag
//!
//! ```no_run
//! use tracing::Level;
//! use tracing_logcat::{LogcatMakeWriter, LogcatTag};
//! use tracing_subscriber::fmt::format::Format;
//!
//! let tag = LogcatTag::Fixed(env!("CARGO_PKG_NAME").to_owned());
//! let writer = LogcatMakeWriter::new(tag)
//!    .expect("Failed to initialize logcat writer");
//!
//! tracing_subscriber::fmt()
//!     .event_format(Format::default().with_level(false).without_time())
//!     .with_writer(writer)
//!     .with_ansi(false)
//!     .with_max_level(Level::TRACE)
//!     .init();
//! ```
//!
//! ### Using the tracing target as the tag
//!
//! ```no_run
//! use tracing::Level;
//! use tracing_logcat::{LogcatMakeWriter, LogcatTag};
//! use tracing_subscriber::fmt::format::Format;
//!
//! let writer = LogcatMakeWriter::new(LogcatTag::Target)
//!    .expect("Failed to initialize logcat writer");
//!
//! tracing_subscriber::fmt()
//!     .event_format(Format::default().with_level(false).with_target(false).without_time())
//!     .with_writer(writer)
//!     .with_ansi(false)
//!     .with_max_level(Level::TRACE)
//!     .init();
//! ```

use std::{
    borrow::Cow,
    io::{self, IoSlice, Write},
    ops::DerefMut,
    os::unix::net::UnixDatagram,
    str,
    sync::Mutex,
    time::SystemTime,
};

use tracing::{Level, Metadata};
use tracing_subscriber::fmt::MakeWriter;

/// Truncate a string so that it doesn't exceed n bytes without producing
/// invalid UTF-8 sequences. However, this does not necessarily truncate at a
/// grapheme cluster boundary.
fn truncate_floor(s: &str, n: usize) -> &str {
    // str::floor_char_boundary() has not been stablized yet.

    let bound = if n >= s.len() {
        s.len()
    } else {
        let lower_bound = n.saturating_sub(3);
        let new_index = (lower_bound..=n).rfind(|i| s.is_char_boundary(*i));

        // SAFETY: str is guaranteed to contain well-formed UTF-8.
        unsafe { new_index.unwrap_unchecked() }
    };

    &s[..bound]
}

enum MaybeUtf8Buf<'a> {
    Bytes(&'a [u8]),
    String(&'a str),
}

impl<'a> MaybeUtf8Buf<'a> {
    fn new(data: &'a [u8]) -> Self {
        match str::from_utf8(data) {
            Ok(s) => Self::String(s),
            Err(_) => Self::Bytes(data),
        }
    }

    fn split_floor(&self, limit: usize) -> (Self, Self) {
        match self {
            Self::Bytes(b) => {
                let chunk = &b[..limit.min(b.len())];
                let remain = &b[chunk.len()..];

                (Self::Bytes(chunk), Self::Bytes(remain))
            }
            Self::String(s) => {
                let chunk = truncate_floor(s, limit);
                let remain = &s[chunk.len()..];

                (Self::String(chunk), Self::String(remain))
            }
        }
    }

    fn as_slice(&self) -> &'a [u8] {
        match self {
            Self::Bytes(b) => b,
            Self::String(s) => s.as_bytes(),
        }
    }
}

/// Iterator that chunks a byte array at the nearest code unit boundary below
/// the specified limit if it is valid UTF-8. If the byte array is not valid
/// UTF-8, then it is split exactly at the limit.
struct Chunker<'a> {
    data: MaybeUtf8Buf<'a>,
    limit: usize,
}

impl<'a> Chunker<'a> {
    fn new(data: &'a [u8], limit: usize) -> Self {
        assert!(
            limit >= 4,
            "Limit cannot be smaller than largest UTF-8 code unit"
        );

        Self {
            data: MaybeUtf8Buf::new(data),
            limit,
        }
    }
}

impl<'a> Iterator for Chunker<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let (chunk, remain) = self.data.split_floor(self.limit);
        let chunk = chunk.as_slice();

        if chunk.is_empty() {
            None
        } else {
            self.data = remain;

            Some(chunk)
        }
    }
}

/// Tag string to use for log messages.
#[derive(Debug, Clone)]
pub enum LogcatTag {
    /// Log all messages with a fixed tag.
    Fixed(String),
    /// Use the `tracing` event target as the tag.
    Target,
}

/// An [`io::Write`] instance that outputs to Android's logcat.
#[derive(Debug)]
pub struct LogcatWriter<'a> {
    socket: &'a Mutex<UnixDatagram>,
    tag: Cow<'a, str>,
    level: Level,
}

impl Write for LogcatWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Implicitly uses LOG_ID_MAIN, which is 0.
        let mut header = [0u8; 11];

        let thread_id = rustix::thread::gettid().as_raw_nonzero();
        header[1..3].copy_from_slice(&(thread_id.get() as u16).to_le_bytes());

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        header[3..7].copy_from_slice(&(timestamp.as_secs() as u32).to_le_bytes());
        header[7..11].copy_from_slice(&timestamp.subsec_nanos().to_le_bytes());

        let priority = match self.level {
            Level::TRACE => [2u8], // ANDROID_LOG_VERBOSE
            Level::DEBUG => [3u8], // ANDROID_LOG_DEBUG
            Level::INFO => [4u8],  // ANDROID_LOG_INFO
            Level::WARN => [5u8],  // ANDROID_LOG_WARN
            Level::ERROR => [6u8], // ANDROID_LOG_ERROR
        };

        // This is truncated to guarantee that we can make progress writing the
        // message.
        let tag = truncate_floor(&self.tag, 128);

        // Everything must be sent as a single datagram.
        let mut iovecs = [
            IoSlice::new(&header),
            IoSlice::new(&priority),
            IoSlice::new(tag.as_bytes()),
            IoSlice::new(&[0]),
            IoSlice::new(&[]),
            IoSlice::new(&[0]),
        ];
        let message_index = 4;

        // Max payload size excludes the header.
        let max_message_len = 4068 - iovecs[1..].iter().map(|v| v.len()).sum::<usize>();

        // Lock once. We don't interleave chunked messages.
        let mut socket = self.socket.lock().unwrap();

        // Remove the implicit trailing newline that tracing adds.
        let no_newline = buf.strip_suffix(b"\n").unwrap_or(buf);

        // Unlike liblog, we split long messages instead of truncating them.
        for chunk in Chunker::new(no_newline, max_message_len) {
            iovecs[message_index] = IoSlice::new(chunk);

            // UnixDatagram does not have a send_vectored().
            let n = rustix::io::writev(socket.deref_mut(), &iovecs)?;
            if n != iovecs.iter().map(|v| v.len()).sum() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "logcat datagram was truncated",
                ));
            }
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A [`MakeWriter`] type that creates [`LogcatWriter`] instances that output to
/// Android's logcat.
#[derive(Debug)]
pub struct LogcatMakeWriter {
    tag: LogcatTag,
    socket: Mutex<UnixDatagram>,
}

impl LogcatMakeWriter {
    /// Return a new instance with the specified tag source.
    pub fn new(tag: LogcatTag) -> io::Result<Self> {
        let socket = UnixDatagram::unbound()?;
        socket.connect("/dev/socket/logdw")?;

        Ok(Self {
            tag,
            socket: Mutex::new(socket),
        })
    }

    fn get_tag(&self, meta: Option<&Metadata>) -> Cow<str> {
        match &self.tag {
            LogcatTag::Fixed(s) => Cow::Borrowed(s),
            LogcatTag::Target => match meta {
                Some(m) => Cow::Owned(m.target().to_owned()),
                None => Cow::Borrowed(""),
            },
        }
    }
}

impl<'a> MakeWriter<'a> for LogcatMakeWriter {
    type Writer = LogcatWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogcatWriter {
            socket: &self.socket,
            tag: self.get_tag(None),
            level: Level::INFO,
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        LogcatWriter {
            socket: &self.socket,
            tag: self.get_tag(Some(meta)),
            level: *meta.level(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunker() {
        let mut chunker = Chunker::new(b"", 4);
        assert_eq!(chunker.next(), None);

        chunker = Chunker::new(b"abcd", 4);
        assert_eq!(chunker.next(), Some(&b"abcd"[..]));
        assert_eq!(chunker.next(), None);

        chunker = Chunker::new(b"foobar", 4);
        assert_eq!(chunker.next(), Some(&b"foob"[..]));
        assert_eq!(chunker.next(), Some(&b"ar"[..]));
        assert_eq!(chunker.next(), None);

        for limit in [4, 5] {
            chunker = Chunker::new("你好".as_bytes(), limit);
            assert_eq!(chunker.next(), Some("你".as_bytes()));
            assert_eq!(chunker.next(), Some("好".as_bytes()));
            assert_eq!(chunker.next(), None);
        }

        chunker = Chunker::new(b"\xffNon-UTF8 \xe4\xbd\xa0\xe5\xa5\xbd", 4);
        assert_eq!(chunker.next(), Some(&b"\xffNon"[..]));
        assert_eq!(chunker.next(), Some(&b"-UTF"[..]));
        assert_eq!(chunker.next(), Some(&b"8 \xe4\xbd"[..]));
        assert_eq!(chunker.next(), Some(&b"\xa0\xe5\xa5\xbd"[..]));
        assert_eq!(chunker.next(), None);
    }
}
