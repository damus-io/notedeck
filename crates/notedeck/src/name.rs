use nostrdb::ProfileRecord;

pub struct NostrName<'a> {
    pub username: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub nip05: Option<&'a str>,
}

impl<'a> NostrName<'a> {
    pub fn name(&self) -> &'a str {
        if let Some(name) = self.username {
            name
        } else if let Some(name) = self.display_name {
            name
        } else {
            self.nip05.unwrap_or("??")
        }
    }

    pub fn unknown() -> Self {
        Self {
            username: None,
            display_name: None,
            nip05: None,
        }
    }
}

fn is_empty(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}

pub fn get_display_name<'a>(record: Option<&ProfileRecord<'a>>) -> NostrName<'a> {
    let Some(record) = record else {
        return NostrName::unknown();
    };

    let Some(profile) = record.record().profile() else {
        return NostrName::unknown();
    };

    let display_name = profile.display_name().filter(|n| !is_empty(n));
    let username = profile.name().filter(|n| !is_empty(n));

    let nip05 = if let Some(raw_nip05) = profile.nip05() {
        if let Some(at_pos) = raw_nip05.find('@') {
            if raw_nip05.starts_with('_') {
                raw_nip05.get(at_pos + 1..)
            } else {
                Some(raw_nip05)
            }
        } else {
            None
        }
    } else {
        None
    };

    NostrName {
        username,
        display_name,
        nip05,
    }
}
