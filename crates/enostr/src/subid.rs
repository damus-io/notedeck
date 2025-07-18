use std::hash::{Hash, Hasher};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SubId {
    /// A subscription id description used for debugging,
    /// since all subids are simply uuids by default for privacy
    description: String,
    id: String,
}

impl PartialEq for SubId {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for SubId {}

impl SubId {
    /// Create a subscription id that is a random uuid. A
    /// description is specified for debugging purposes
    pub fn new(description: String) -> Self {
        let id = Uuid::new_v4().to_string();
        Self { description, id }
    }

    /// Create a subscription id with a specific string
    /// instead of a random uuid
    pub fn from_string(id: String, description: String) -> Self {
        Self { id, description }
    }

    pub fn to_str(&self) -> &str {
        &self.id
    }

    pub fn to_string(&self) -> String {
        self.id.clone()
    }

    pub fn description(&self) -> &str {
        &self.description
    }
}

impl std::fmt::Display for SubId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // we don't really care to display the underlying uuid...
        write!(
            f,
            "SubId('{}', {}...)",
            self.description,
            abbrev_str(&self.id, 8)
        )
    }
}

impl Hash for SubId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

fn abbrev_str(s: &str, len: usize) -> &str {
    let should_abbrev = s.len() > len;
    if should_abbrev {
        let closest = floor_char_boundary(s, len);
        &s[..closest]
    } else {
        s
    }
}

#[inline]
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        s.len()
    } else {
        let lower_bound = index.saturating_sub(3);
        let new_index = s.as_bytes()[lower_bound..=index]
            .iter()
            .rposition(|b| is_utf8_char_boundary(*b));

        // SAFETY: we know that the character boundary will be within four bytes
        unsafe { lower_bound + new_index.unwrap_unchecked() }
    }
}

#[inline]
fn is_utf8_char_boundary(c: u8) -> bool {
    // This is bit magic equivalent to: b < 128 || b >= 192
    (c as i8) >= -0x40
}
