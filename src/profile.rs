use nostrdb::ProfileRecord;

pub enum DisplayName<'a> {
    One(&'a str),

    Both {
        username: &'a str,
        display_name: &'a str,
    },
}

impl<'a> DisplayName<'a> {
    pub fn username(&self) -> &'a str {
        match self {
            Self::One(n) => n,
            Self::Both { username, .. } => username,
        }
    }
}

fn is_empty(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}

pub fn get_profile_name<'a>(record: &'a ProfileRecord) -> Option<DisplayName<'a>> {
    let profile = record.record().profile()?;
    let display_name = profile.display_name().filter(|n| !is_empty(n));
    let name = profile.name().filter(|n| !is_empty(n));

    match (display_name, name) {
        (None, None) => None,
        (Some(disp), None) => Some(DisplayName::One(disp)),
        (None, Some(username)) => Some(DisplayName::One(username)),
        (Some(display_name), Some(username)) => Some(DisplayName::Both {
            display_name,
            username,
        }),
    }
}
