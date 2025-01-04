use nostrdb::{NdbProfile, ProfileRecord};

#[derive(Default, Debug)]
pub struct ProfileState {
    pub display_name: String,
    pub name: String,
    pub picture: String,
    pub banner: String,
    pub about: String,
    pub website: String,
    pub lud16: String,
    pub nip05: String,
}

impl ProfileState {
    pub fn from_profile(record: &ProfileRecord<'_>) -> Self {
        let display_name = get_item(record, |p| p.display_name());
        let username = get_item(record, |p| p.name());
        let profile_picture = get_item(record, |p| p.picture());
        let cover_image = get_item(record, |p| p.banner());
        let about = get_item(record, |p| p.about());
        let website = get_item(record, |p| p.website());
        let lud16 = get_item(record, |p| p.lud16());
        let nip05 = get_item(record, |p| p.nip05());

        Self {
            display_name,
            name: username,
            picture: profile_picture,
            banner: cover_image,
            about,
            website,
            lud16,
            nip05,
        }
    }

    pub fn to_json(&self) -> String {
        let mut fields = Vec::new();

        if !self.display_name.is_empty() {
            fields.push(format!(r#""display_name":"{}""#, self.display_name));
        }
        if !self.name.is_empty() {
            fields.push(format!(r#""name":"{}""#, self.name));
        }
        if !self.picture.is_empty() {
            fields.push(format!(r#""picture":"{}""#, self.picture));
        }
        if !self.banner.is_empty() {
            fields.push(format!(r#""banner":"{}""#, self.banner));
        }
        if !self.about.is_empty() {
            fields.push(format!(r#""about":"{}""#, self.about));
        }
        if !self.website.is_empty() {
            fields.push(format!(r#""website":"{}""#, self.website));
        }
        if !self.lud16.is_empty() {
            fields.push(format!(r#""lud16":"{}""#, self.lud16));
        }
        if !self.nip05.is_empty() {
            fields.push(format!(r#""nip05":"{}""#, self.nip05));
        }

        format!("{{{}}}", fields.join(","))
    }
}

fn get_item<'a>(
    record: &ProfileRecord<'a>,
    item_retriever: fn(NdbProfile<'a>) -> Option<&'a str>,
) -> String {
    record
        .record()
        .profile()
        .and_then(item_retriever)
        .map_or_else(String::new, ToString::to_string)
}
