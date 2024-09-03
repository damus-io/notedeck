use std::collections::HashMap;

pub enum SubKind {
    /// Initial subscription. This is the first time we do a remote subscription
    /// for a timeline
    Initial,

    /// One shot requests, we can just close after we receive EOSE
    OneShot,

    /// We are fetching a contact list so that we can use it for our follows
    /// Filter.
    // TODO: generalize this to any list?
    FetchingContactList(u32),
}

/// Subscriptions that need to be tracked at various stages. Sometimes we
/// need to do A, then B, then C. Tracking requests at various stages by
/// mapping uuid subids to explicit states happens here.
#[derive(Default)]
pub struct Subscriptions {
    pub subs: HashMap<String, SubKind>,
}
