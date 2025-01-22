use std::cmp::Ordering;
use std::fmt;

// A Relay specification includes NIP-65 defined "markers" which
// indicate if the relay should be used for reading or writing (or
// both).

#[derive(Clone)]
pub struct RelaySpec {
    pub url: String,
    pub has_read_marker: bool,
    pub has_write_marker: bool,
}

impl RelaySpec {
    pub fn new(
        url: impl Into<String>,
        mut has_read_marker: bool,
        mut has_write_marker: bool,
    ) -> Self {
        // if both markers are set turn both off ...
        if has_read_marker && has_write_marker {
            has_read_marker = false;
            has_write_marker = false;
        }
        RelaySpec {
            url: url.into(),
            has_read_marker,
            has_write_marker,
        }
    }

    // The "marker" fields are a little counter-intuitive ... from NIP-65:
    //
    // "The event MUST include a list of r tags with relay URIs and a read
    // or write marker. Relays marked as read / write are called READ /
    // WRITE relays, respectively. If the marker is omitted, the relay is
    // used for both purposes."
    //
    pub fn is_readable(&self) -> bool {
        !self.has_write_marker // only "write" relays are not readable
    }
    pub fn is_writable(&self) -> bool {
        !self.has_read_marker // only "read" relays are not writable
    }
}

// just the url part
impl fmt::Display for RelaySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

// add the read and write markers if present
impl fmt::Debug for RelaySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self)?;
        if self.has_read_marker {
            write!(f, " [r]")?;
        }
        if self.has_write_marker {
            write!(f, " [w]")?;
        }
        Ok(())
    }
}

// For purposes of set arithmetic only the url is considered, two
// RelaySpec which differ only in markers are the same ...

impl PartialEq for RelaySpec {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
    }
}

impl Eq for RelaySpec {}

impl PartialOrd for RelaySpec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.url.cmp(&other.url))
    }
}

impl Ord for RelaySpec {
    fn cmp(&self, other: &Self) -> Ordering {
        self.url.cmp(&other.url)
    }
}
