use std::cmp::Ordering;
use std::fmt;

use enostr::NormRelayUrl;
use nostrdb::Note;

// A Relay specification includes NIP-65 defined "markers" which
// indicate if the relay should be used for reading or writing (or
// both).

#[derive(Clone)]
pub struct RelaySpec {
    pub url: NormRelayUrl,
    pub has_read_marker: bool,
    pub has_write_marker: bool,
}

impl RelaySpec {
    pub fn new(url: NormRelayUrl, mut has_read_marker: bool, mut has_write_marker: bool) -> Self {
        // if both markers are set turn both off ...
        if has_read_marker && has_write_marker {
            has_read_marker = false;
            has_write_marker = false;
        }
        RelaySpec {
            url,
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

/// Parses NIP-65 relay specs from a kind-10002 note body.
pub(crate) fn relays_from_nip65_note(note: &Note<'_>) -> Vec<RelaySpec> {
    let mut relays = Vec::new();
    for tag in note.tags() {
        if tag.get(0).and_then(|t| t.variant().str()) != Some("r") {
            continue;
        }

        let Some(url) = tag.get(1).and_then(|f| f.variant().str()) else {
            continue;
        };

        let has_read_marker = tag
            .get(2)
            .is_some_and(|m| m.variant().str() == Some("read"));
        let has_write_marker = tag
            .get(2)
            .is_some_and(|m| m.variant().str() == Some("write"));
        let Ok(norm_url) = NormRelayUrl::new(url) else {
            continue;
        };
        relays.push(RelaySpec::new(norm_url, has_read_marker, has_write_marker));
    }
    relays
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
        write!(f, "\"{self}\"")?;
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

#[allow(clippy::non_canonical_partial_ord_impl)]
impl PartialOrd for RelaySpec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.url.to_string().cmp(&other.url.to_string()))
    }
}

impl Ord for RelaySpec {
    fn cmp(&self, other: &Self) -> Ordering {
        self.url.to_string().cmp(&other.url.to_string())
    }
}
