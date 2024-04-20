use nostrdb::ProfileRecord;

pub fn get_profile_name<'a>(record: &'a ProfileRecord) -> Option<&'a str> {
    let profile = record.record().profile()?;
    let display_name = profile.display_name();
    let name = profile.name();

    if display_name.is_some() && display_name.unwrap() != "" {
        return display_name;
    }

    if name.is_some() && name.unwrap() != "" {
        return name;
    }

    None
}
