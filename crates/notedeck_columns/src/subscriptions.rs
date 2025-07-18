use crate::timeline::TimelineKind;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum AccountSubKind {
    Mutes,
    Contacts,
}

#[derive(Debug, Clone)]
pub enum SubKind {
    /// Initial subscription. This is the first time we do a remote subscription
    /// for a timeline
    Initial,

    /// One shot requests, we can just close after we receive EOSE
    OneShot,

    /// Home subs
    AccountSub(AccountSubKind),

    /// Timeline subs
    Timeline(TimelineKind),

    /// We are fetching a contact list so that we can use it for our follows
    /// Filter.
    // TODO: generalize this to any list?
    FetchingContactList(TimelineKind),
}

/// Subscriptions that need to be tracked at various stages. Sometimes we
/// need to do A, then B, then C. Tracking requests at various stages by
/// mapping uuid subids to explicit states happens here.
#[derive(Default)]
pub struct Subscriptions {
    pub subs: HashMap<String, SubKind>,
}
