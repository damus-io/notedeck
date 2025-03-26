use nostrdb::ProfileRecord;

pub mod picture;

pub use picture::ProfilePic;

pub fn get_profile_url<'a>(profile: Option<&ProfileRecord<'a>>) -> &'a str {
    unwrap_profile_url(profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())))
}

pub fn unwrap_profile_url(maybe_url: Option<&str>) -> &str {
    if let Some(url) = maybe_url {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}
