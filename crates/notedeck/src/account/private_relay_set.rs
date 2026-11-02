//! The account's kind-10013 private relays, each with the role it plays.
//!
//! A private relay either carries everything the account syncs privately, or
//! only the encrypted envelopes (kind-1059 gift-wraps, kind-1080 PNS and
//! kind-1081 SNS envelopes). The second kind is a relay the account shares with
//! other people, such as a team's rendezvous relay: it must never see the
//! account's plaintext app events, nor the 10013 itself.

use std::collections::BTreeMap;

use enostr::{NormRelayUrl, RelayId};

/// What a private relay may be sent. Stored as the optional third element of
/// the 10013's `["relay", url, "encrypted"]` private tag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrivateRelayRole {
    /// Plaintext private app events as well as encrypted envelopes. A 10013
    /// tag with no role element (every list written before roles existed, and
    /// Amethyst's) means this.
    #[default]
    Full,
    /// Only encrypted envelopes (1059/1080/1081); plaintext never goes here.
    EncryptedOnly,
}

/// The kind of a NIP-59 gift-wrap, the carrier of a kind-1082 key-share.
const GIFT_WRAP_KIND: u32 = 1059;

/// Whether `kind` is an encrypted envelope — a kind-1059 gift-wrap, a kind-1080
/// PNS note or a kind-1081 SNS envelope — and so may go to an encrypted-only
/// private relay. Every other kind is plaintext to the relay that stores it.
pub fn is_envelope_kind(kind: u32) -> bool {
    kind == GIFT_WRAP_KIND
        || kind == nostrdb_net::pns::PNS_KIND
        || kind == nostrdb_net::sns::SNS_ENVELOPE_KIND
}

/// The 10013 tag element that marks [`PrivateRelayRole::EncryptedOnly`].
pub(crate) const ENCRYPTED_ONLY_MARKER: &str = "encrypted";

impl PrivateRelayRole {
    /// Read a role from the 10013 tag's third element. Anything but the
    /// encrypted-only marker, including no element, is [`Self::Full`].
    pub(crate) fn from_marker(marker: Option<&str>) -> Self {
        if marker == Some(ENCRYPTED_ONLY_MARKER) {
            Self::EncryptedOnly
        } else {
            Self::Full
        }
    }

    /// The 10013 tag's third element for this role, `None` for a full relay so
    /// its tag stays the plain two-element form other clients write.
    pub(crate) fn marker(self) -> Option<&'static str> {
        match self {
            Self::Full => None,
            Self::EncryptedOnly => Some(ENCRYPTED_ONLY_MARKER),
        }
    }

    /// Whether a relay with this role may be sent plaintext.
    pub fn carries_plaintext(self) -> bool {
        self == Self::Full
    }
}

/// One private relay and its role: an entry of a [`PrivateRelaySet`], and what
/// a parsed 10013 yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateRelay {
    pub url: NormRelayUrl,
    pub role: PrivateRelayRole,
}

/// The account's private relays keyed by URL, each with its
/// [`PrivateRelayRole`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivateRelaySet {
    roles: BTreeMap<NormRelayUrl, PrivateRelayRole>,
}

impl PrivateRelaySet {
    /// Whether `url` is one of the private relays, whatever its role.
    pub fn contains(&self, url: &NormRelayUrl) -> bool {
        self.roles.contains_key(url)
    }

    /// The role of `url`, or `None` when it isn't a private relay.
    pub fn role(&self, url: &NormRelayUrl) -> Option<PrivateRelayRole> {
        self.roles.get(url).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }

    /// Every private relay, sorted by URL: where encrypted envelopes go.
    pub fn urls(&self) -> impl Iterator<Item = &NormRelayUrl> {
        self.roles.keys()
    }

    /// The private relays that may carry plaintext, sorted by URL.
    pub fn plaintext_urls(&self) -> impl Iterator<Item = &NormRelayUrl> {
        self.roles
            .iter()
            .filter(|(_, role)| role.carries_plaintext())
            .map(|(url, _)| url)
    }

    /// Every private relay with its role, sorted by URL.
    pub fn iter(&self) -> impl Iterator<Item = PrivateRelay> + '_ {
        self.roles.iter().map(|(url, role)| PrivateRelay {
            url: url.clone(),
            role: *role,
        })
    }

    /// Every private relay as a websocket [`RelayId`].
    pub fn relay_ids(&self) -> Vec<RelayId> {
        self.urls().cloned().map(RelayId::Websocket).collect()
    }

    /// The plaintext-carrying private relays as websocket [`RelayId`]s.
    pub fn plaintext_relay_ids(&self) -> Vec<RelayId> {
        self.plaintext_urls()
            .cloned()
            .map(RelayId::Websocket)
            .collect()
    }

    /// Both target lists at once, for a publisher that routes each event by
    /// its kind ([`PrivateRelayTargets::for_kind`]).
    pub fn targets(&self) -> PrivateRelayTargets {
        PrivateRelayTargets {
            envelopes: self.relay_ids(),
            plaintext: self.plaintext_relay_ids(),
        }
    }

    /// Add `url` as a [`PrivateRelayRole::Full`] relay. A relay already in the
    /// set keeps its role, so re-adding an encrypted-only relay can't quietly
    /// open it to plaintext.
    pub(crate) fn add(&mut self, url: NormRelayUrl) {
        self.roles.entry(url).or_default();
    }

    /// Insert `relay`, replacing any role `url` had. For a parsed 10013.
    pub(crate) fn insert(&mut self, relay: PrivateRelay) {
        self.roles.insert(relay.url, relay.role);
    }

    pub(crate) fn remove(&mut self, url: &NormRelayUrl) {
        self.roles.remove(url);
    }

    /// Set the role of a relay already in the set. Returns `false`, changing
    /// nothing, when `url` isn't a private relay or already has `role`.
    pub(crate) fn set_role(&mut self, url: &NormRelayUrl, role: PrivateRelayRole) -> bool {
        match self.roles.get_mut(url) {
            Some(current) if *current != role => {
                *current = role;
                true
            }
            _ => false,
        }
    }
}

/// The private relays split by what each may carry, so a publisher that emits
/// both plaintext events and encrypted envelopes — a headway write is either,
/// depending on whether its board is shared — sends each only where it may go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivateRelayTargets {
    /// Every private relay: where an envelope ([`is_envelope_kind`]) goes.
    pub envelopes: Vec<RelayId>,
    /// The private relays that may carry plaintext: where any other kind goes.
    pub plaintext: Vec<RelayId>,
}

impl PrivateRelayTargets {
    /// The relays an event of `kind` may be sent to.
    pub fn for_kind(&self, kind: u32) -> &[RelayId] {
        if is_envelope_kind(kind) {
            &self.envelopes
        } else {
            &self.plaintext
        }
    }

    /// No private relay at all: nothing would be sent anywhere.
    pub fn is_empty(&self) -> bool {
        self.envelopes.is_empty()
    }
}

impl FromIterator<PrivateRelay> for PrivateRelaySet {
    fn from_iter<I: IntoIterator<Item = PrivateRelay>>(iter: I) -> Self {
        let mut set = Self::default();
        for relay in iter {
            set.insert(relay);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> NormRelayUrl {
        NormRelayUrl::new(s).expect("relay url")
    }

    fn set() -> PrivateRelaySet {
        [
            PrivateRelay {
                url: url("wss://home.example.com"),
                role: PrivateRelayRole::Full,
            },
            PrivateRelay {
                url: url("wss://sns.example.com"),
                role: PrivateRelayRole::EncryptedOnly,
            },
        ]
        .into_iter()
        .collect()
    }

    /// Envelopes go to every private relay; plaintext skips encrypted-only ones.
    #[test]
    fn plaintext_urls_exclude_encrypted_only() {
        let set = set();
        assert_eq!(
            set.urls().collect::<Vec<_>>(),
            vec![
                &url("wss://home.example.com"),
                &url("wss://sns.example.com")
            ]
        );
        assert_eq!(
            set.plaintext_urls().collect::<Vec<_>>(),
            vec![&url("wss://home.example.com")]
        );
        assert_eq!(
            set.plaintext_relay_ids(),
            vec![RelayId::Websocket(url("wss://home.example.com"))]
        );
    }

    /// Re-adding an encrypted-only relay keeps it encrypted-only.
    #[test]
    fn add_keeps_an_existing_role() {
        let mut set = set();
        set.add(url("wss://sns.example.com"));
        assert_eq!(
            set.role(&url("wss://sns.example.com")),
            Some(PrivateRelayRole::EncryptedOnly)
        );
        set.add(url("wss://new.example.com"));
        assert_eq!(
            set.role(&url("wss://new.example.com")),
            Some(PrivateRelayRole::Full)
        );
    }

    /// A role is only set on a relay in the set, and only reports a change.
    #[test]
    fn set_role_reports_changes_only() {
        let mut set = set();
        let home = url("wss://home.example.com");
        assert!(set.set_role(&home, PrivateRelayRole::EncryptedOnly));
        assert!(!set.set_role(&home, PrivateRelayRole::EncryptedOnly));
        assert!(!set.set_role(
            &url("wss://absent.example.com"),
            PrivateRelayRole::EncryptedOnly
        ));
        assert!(!set.contains(&url("wss://absent.example.com")));
        assert_eq!(set.plaintext_urls().count(), 0);
    }

    /// An envelope may go to every private relay; anything else only to the
    /// plaintext ones.
    #[test]
    fn targets_route_by_kind() {
        let targets = set().targets();
        let home = vec![RelayId::Websocket(url("wss://home.example.com"))];
        let both = vec![
            RelayId::Websocket(url("wss://home.example.com")),
            RelayId::Websocket(url("wss://sns.example.com")),
        ];
        for kind in [1059, 1080, 1081] {
            assert_eq!(targets.for_kind(kind), both.as_slice(), "kind {kind}");
        }
        for kind in [1, 1082, 10013, 30620, 31922] {
            assert_eq!(targets.for_kind(kind), home.as_slice(), "kind {kind}");
        }
    }

    /// Only the exact marker means encrypted-only; no element means full.
    #[test]
    fn role_marker_round_trips() {
        for role in [PrivateRelayRole::Full, PrivateRelayRole::EncryptedOnly] {
            assert_eq!(PrivateRelayRole::from_marker(role.marker()), role);
        }
        assert_eq!(
            PrivateRelayRole::from_marker(Some("read")),
            PrivateRelayRole::Full
        );
    }
}
