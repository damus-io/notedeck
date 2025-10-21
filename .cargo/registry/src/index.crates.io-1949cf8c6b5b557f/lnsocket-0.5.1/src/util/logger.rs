// Pruned copy of crate rust log, without global logger
// https://github.com/rust-lang-nursery/log #7a60286
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Log traits live here, which are called throughout the library to provide useful information for
//! debugging purposes.
//!
//! Log messages should be filtered client-side by implementing check against a given [`Record`]'s
//! [`Level`] field. Each module may have its own Logger or share one.

use bitcoin::secp256k1::PublicKey;

use core::cmp;
use core::fmt;
use core::ops::Deref;

static LOG_LEVEL_NAMES: [&str; 6] = ["GOSSIP", "TRACE", "DEBUG", "INFO", "WARN", "ERROR"];

/// An enum representing the available verbosity levels of the logger.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Level {
    /// Designates extremely verbose information, including gossip-induced messages
    Gossip,
    /// Designates very low priority, often extremely verbose, information
    Trace,
    /// Designates lower priority information
    Debug,
    /// Designates useful information
    Info,
    /// Designates hazardous situations
    Warn,
    /// Designates very serious errors
    Error,
}

impl PartialOrd for Level {
    #[inline]
    fn partial_cmp(&self, other: &Level) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }

    #[inline]
    fn lt(&self, other: &Level) -> bool {
        (*self as usize) < *other as usize
    }

    #[inline]
    fn le(&self, other: &Level) -> bool {
        *self as usize <= *other as usize
    }

    #[inline]
    fn gt(&self, other: &Level) -> bool {
        *self as usize > *other as usize
    }

    #[inline]
    fn ge(&self, other: &Level) -> bool {
        *self as usize >= *other as usize
    }
}

impl Ord for Level {
    #[inline]
    fn cmp(&self, other: &Level) -> cmp::Ordering {
        (*self as usize).cmp(&(*other as usize))
    }
}

impl fmt::Display for Level {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.pad(LOG_LEVEL_NAMES[*self as usize])
    }
}

impl Level {
    /// Returns the most verbose logging level.
    #[inline]
    pub fn max() -> Level {
        Level::Gossip
    }
}

/// A Record, unit of logging output with Metadata to enable filtering
/// Module_path, file, line to inform on log's source
#[derive(Clone, Debug)]
pub struct Record {
    /// The verbosity level of the message.
    ///pub level: Level,
    /// The node id of the peer pertaining to the logged record.
    ///
    /// Note that in some cases a [`Self::channel_id`] may be filled in but this may still be
    /// `None`, depending on if the peer information is readily available in LDK when the log is
    /// generated.
    pub peer_id: Option<PublicKey>,
    // The message body.
    //pub args: fmt::Arguments<'a>,
    // The module path of the message.
    //pub module_path: &'static str,
    // The source file containing the message.
    //pub file: &'static str,
    // The line containing the message.
    //pub line: u32,
}

/// A trait encapsulating the operations required of a logger.
pub trait Logger {
    /// Logs the [`Record`].
    fn log(&self, record: Record);
}

/// Adds relevant context to a [`Record`] before passing it to the wrapped [`Logger`].
///
/// This is not exported to bindings users as lifetimes are problematic and there's little reason
/// for this to be used downstream anyway.
pub struct WithContext<'a, L: Deref>
where
    L::Target: Logger,
{
    /// The logger to delegate to after adding context to the record.
    logger: &'a L,
    /// The node id of the peer pertaining to the logged record.
    peer_id: Option<PublicKey>,
}

impl<'a, L: Deref> Logger for WithContext<'a, L>
where
    L::Target: Logger,
{
    fn log(&self, mut record: Record) {
        if self.peer_id.is_some() {
            record.peer_id = self.peer_id
        };
        self.logger.log(record)
    }
}

/// Wrapper for logging a [`PublicKey`] in hex format.
///
/// This is not exported to bindings users as fmt can't be used in C
#[doc(hidden)]
pub struct DebugPubKey<'a>(pub &'a PublicKey);
impl<'a> core::fmt::Display for DebugPubKey<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
        for i in self.0.serialize().iter() {
            write!(f, "{:02x}", i)?;
        }
        Ok(())
    }
}

/// Wrapper for logging byte slices in hex format.
///
/// This is not exported to bindings users as fmt can't be used in C
#[doc(hidden)]
pub struct DebugBytes<'a>(pub &'a [u8]);
impl<'a> core::fmt::Display for DebugBytes<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> Result<(), core::fmt::Error> {
        for i in self.0 {
            write!(f, "{:02x}", i)?;
        }
        Ok(())
    }
}

/// Wrapper for logging `Iterator`s.
///
/// This is not exported to bindings users as fmt can't be used in C
#[doc(hidden)]
pub struct DebugIter<T: fmt::Display, I: core::iter::Iterator<Item = T> + Clone>(pub I);
impl<T: fmt::Display, I: core::iter::Iterator<Item = T> + Clone> fmt::Display for DebugIter<T, I> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "[")?;
        let mut iter = self.0.clone();
        if let Some(item) = iter.next() {
            write!(f, "{}", item)?;
        }
        for item in iter {
            write!(f, ", {}", item)?;
        }
        write!(f, "]")?;
        Ok(())
    }
}
