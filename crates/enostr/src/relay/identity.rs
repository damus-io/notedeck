use std::{
    borrow::Borrow,
    fmt::{self, Display},
};

use hashbrown::HashSet;
use nostr::types::RelayUrl;
use url::{Host, Url};
use uuid::Uuid;

use crate::Error;

#[derive(Eq, PartialEq, Hash, Clone, Debug)]
pub enum RelayId {
    Websocket(NormRelayUrl),
    Multicast,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct OutboxSubId(pub u64);

#[derive(Clone, Copy, Hash, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct FullHistorySubId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RelayReqStatus {
    InitialQuery,
    Eose,
    Closed,
}

/// Readiness of one desired relay leg for an outbox subscription.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RelayLegReadiness {
    /// A relay-local REQ exists and exposes protocol status.
    Placed(RelayReqStatus),
    /// The relay is still eligible, but no relay-local REQ exists yet.
    PendingPlacement,
    /// The relay is not eligible for the current subscription transport.
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelayReqId(pub String);

impl RelayReqId {
    pub fn byte_len() -> usize {
        uuid::fmt::Hyphenated::LENGTH
    }
}

impl Default for RelayReqId {
    fn default() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl From<String> for RelayReqId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<RelayReqId> for String {
    fn from(value: RelayReqId) -> Self {
        value.0
    }
}

impl From<&str> for RelayReqId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<Uuid> for RelayReqId {
    fn from(value: Uuid) -> Self {
        RelayReqId(value.to_string())
    }
}

impl std::fmt::Display for RelayReqId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Borrow<str> for RelayReqId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, PartialOrd, Ord)]
pub struct NormRelayUrl {
    url: RelayUrl,
}

impl NormRelayUrl {
    pub fn new(url: &str) -> Result<Self, Error> {
        Ok(Self {
            url: nostr::RelayUrl::parse(canonicalize_url(url.to_owned()))
                .map_err(|_| Error::InvalidRelayUrl)?,
        })
    }

    /// Return the canonical relay URL serialization.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Return whether this relay URL may be used for the given relay URL source.
    pub fn allowed_for_source(&self, source: RelayUrlSource) -> bool {
        match source {
            RelayUrlSource::Explicit => true,
            RelayUrlSource::RemoteAdvertised => self.allowed_remote_advertised_endpoint(),
        }
    }

    fn allowed_remote_advertised_endpoint(&self) -> bool {
        let url: &Url = (&self.url).into();
        if !matches!(url.scheme(), "ws" | "wss") {
            return false;
        }

        if !remote_advertised_url_parts_allowed(url) {
            return false;
        }

        let Some(host) = url.host() else {
            return false;
        };

        match host {
            Host::Domain(domain) => public_domain_host_allowed(domain),
            Host::Ipv4(_) | Host::Ipv6(_) => false,
        }
    }
}

impl Display for NormRelayUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

impl From<NormRelayUrl> for RelayUrl {
    fn from(value: NormRelayUrl) -> Self {
        value.url
    }
}

impl From<RelayUrl> for NormRelayUrl {
    fn from(url: RelayUrl) -> Self {
        Self { url }
    }
}

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub enum RelayType {
    Compaction,
    Transparent,
}

/// Caller-declared importance for one subscription's relay websocket demand.
///
/// `OutboxPool` uses this value only for websocket admission and eviction under
/// fd pressure. It does not decide whether the subscription exists: deferred
/// relay legs keep their declared demand and can be retried when capacity
/// returns.
///
/// During normal operation lower-value demand can open until the projected fd
/// stop watermark. At that watermark, and while soft pressure remains active,
/// `BestEffort` and `Opportunistic` opens are deferred while `Important` and
/// `Critical` opens remain admissible. Under a hard fd-exhaustion signal, or
/// when the configured websocket cap would be exceeded, any prioritized open
/// requires first evicting a lower-value websocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RelayDemandPriority {
    /// Lowest-value demand.
    ///
    /// Intended for hidden discovery or maintenance work. This is the first
    /// class to defer near fd limits and the first class eligible for eviction.
    BestEffort,
    /// Additive coverage that improves recall without baseline correctness.
    ///
    /// This can open while fd pressure is clear, but is deferred under soft
    /// pressure and may be evicted to preserve `Important` or `Critical` demand.
    Opportunistic,
    /// Baseline demand that should be preserved under normal fd scarcity.
    ///
    /// Under soft fd pressure this demand may still open a websocket after
    /// opportunistically evicting lower-value relay demand. Under hard pressure
    /// or the configured websocket cap it can open only after a lower-value
    /// websocket is actually evicted.
    Important,
    /// Highest-value demand.
    ///
    /// This has the same fd-admission shape as `Important`, but outranks
    /// `Important` when choosing which existing websocket can be evicted.
    Critical,
}

/// Effective relay connection priority derived from active relay demand.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RelayConnectionPriority {
    pub strongest_demand: RelayDemandPriority,
    pub request_count: usize,
}

impl RelayConnectionPriority {
    /// Builds a relay priority from one demand class and non-zero work count.
    pub(in crate::relay) fn from_demand(
        strongest_demand: RelayDemandPriority,
        request_count: usize,
    ) -> Option<Self> {
        (request_count > 0).then_some(Self {
            strongest_demand,
            request_count,
        })
    }

    /// Merges two relay priorities, preserving strongest demand and total work.
    pub(in crate::relay) fn merge(self, other: Self) -> Self {
        Self {
            strongest_demand: self.strongest_demand.max(other.strongest_demand),
            request_count: self.request_count.saturating_add(other.request_count),
        }
    }
}

/// Caller intent for how a subscription should be routed when relay capacity is constrained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RelayRoutingPreference {
    /// The subscription must use a dedicated relay subscription.
    /// If a dedicated slot cannot be obtained immediately, it is queued for
    /// dedicated retry (no compaction fallback).
    RequireDedicated,
    /// Prefer a dedicated relay subscription, but allow compaction fallback.
    #[default]
    PreferDedicated,
    /// No dedicated-vs-compaction preference.
    /// The coordinator may demote this class first under contention.
    NoPreference,
}

impl RelayRoutingPreference {
    /// Returns the stronger routing preference for scarce dedicated capacity.
    pub(in crate::relay) fn strongest(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    /// Ordering used when merging relay package policies.
    fn rank(self) -> u8 {
        match self {
            Self::NoPreference => 0,
            Self::PreferDedicated => 1,
            Self::RequireDedicated => 2,
        }
    }
}

/// Trust source for a package of relay URLs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RelayUrlSource {
    /// Relay URLs were supplied by local account/user configuration or explicit caller intent.
    #[default]
    Explicit,
    /// Relay URLs came from a remote-authored relay list and must pass syntactic public endpoint policy.
    ///
    /// Domain names are filtered by public-domain syntax here; this does not
    /// resolve DNS or prove that a domain resolves only to public IP addresses.
    RemoteAdvertised,
}

impl RelayUrlSource {
    /// Merge relay URL trust sources without weakening explicit local intent.
    pub(in crate::relay) fn strongest(self, other: Self) -> Self {
        if matches!(self, Self::Explicit) || matches!(other, Self::Explicit) {
            Self::Explicit
        } else {
            Self::RemoteAdvertised
        }
    }
}

/// Relay URL package plus per-subscription routing preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayUrlPolicy {
    /// Trust source used when applying relay URL endpoint policy.
    source: RelayUrlSource,
    /// Caller-declared value of opening or keeping relay connections for this
    /// subscription.
    demand_priority: RelayDemandPriority,
    /// Preferred routing behavior when dedicated capacity is scarce.
    routing_preference: RelayRoutingPreference,
    /// Caller-provided tie-breaker for relays in the same demand/source class.
    connection_weight: u32,
}

impl RelayUrlPolicy {
    /// Construct policy for locally configured or caller-explicit relay URLs.
    pub fn explicit(
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self::new(
            RelayUrlSource::Explicit,
            demand_priority,
            routing_preference,
        )
    }

    /// Construct policy for relays learned from remote-authored relay lists.
    pub fn remote_advertised(
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self::new(
            RelayUrlSource::RemoteAdvertised,
            demand_priority,
            routing_preference,
        )
    }

    /// Construct policy with all scarcity axes explicit.
    pub fn new(
        source: RelayUrlSource,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self {
            source,
            demand_priority,
            routing_preference,
            connection_weight: 0,
        }
    }

    /// Return the relay URL source used by endpoint policy.
    pub fn source(self) -> RelayUrlSource {
        self.source
    }

    /// Return the caller-declared relay connection demand priority.
    pub fn demand_priority(self) -> RelayDemandPriority {
        self.demand_priority
    }

    /// Return the preferred routing behavior under dedicated capacity pressure.
    pub fn routing_preference(self) -> RelayRoutingPreference {
        self.routing_preference
    }

    /// Return the caller-provided relay connection tie-breaker.
    pub fn connection_weight(self) -> u32 {
        self.connection_weight
    }

    /// Return this policy with a caller-provided connection tie-breaker.
    pub fn with_connection_weight(mut self, connection_weight: u32) -> Self {
        self.connection_weight = connection_weight;
        self
    }

    /// Merge another policy without weakening local intent or scarcity demand.
    pub(in crate::relay) fn merge_from(&mut self, other: Self) {
        self.source = self.source.strongest(other.source);
        self.demand_priority = self.demand_priority.max(other.demand_priority);
        self.routing_preference = self.routing_preference.strongest(other.routing_preference);
        self.connection_weight = self.connection_weight.max(other.connection_weight);
    }
}

/// Relay URL package plus per-subscription routing preference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayUrlPkgs {
    /// Target relay URLs for this subscription.
    pub(in crate::relay) urls: HashSet<NormRelayUrl>,
    /// Policy used when filtering relay URLs and routing relay demand.
    pub(in crate::relay) policy: RelayUrlPolicy,
}

impl RelayUrlPkgs {
    /// Construct a relay package with explicit routing policy.
    pub fn new(urls: HashSet<NormRelayUrl>, policy: RelayUrlPolicy) -> Self {
        Self { urls, policy }
    }

    /// Construct a single-relay package with explicit routing policy.
    pub(in crate::relay) fn single(relay: NormRelayUrl, policy: RelayUrlPolicy) -> Self {
        Self {
            urls: HashSet::from([relay]),
            policy,
        }
    }

    /// Return the package policy.
    pub fn policy(&self) -> RelayUrlPolicy {
        self.policy
    }

    /// Return a package with the same policy and only one relay URL.
    pub(in crate::relay) fn single_relay_with_same_policy(&self, relay: NormRelayUrl) -> Self {
        Self {
            urls: HashSet::from([relay]),
            policy: self.policy,
        }
    }

    /// Merge another package's policy into this one without dropping relay URLs.
    pub(in crate::relay) fn merge_policy_from(&mut self, other: &Self) {
        self.urls.extend(other.urls.iter().cloned());
        self.policy.merge_from(other.policy);
    }

    pub fn iter(&self) -> impl Iterator<Item = &NormRelayUrl> {
        self.urls.iter()
    }

    /// Return the target relay URLs for this subscription.
    pub fn urls(&self) -> &HashSet<NormRelayUrl> {
        &self.urls
    }

    /// Return the trust source used when applying relay URL endpoint policy.
    pub fn source(&self) -> RelayUrlSource {
        self.policy.source()
    }

    /// Return the caller-declared relay connection demand priority.
    pub fn demand_priority(&self) -> RelayDemandPriority {
        self.policy.demand_priority()
    }

    /// Return the preferred routing behavior under dedicated capacity pressure.
    pub fn routing_preference(&self) -> RelayRoutingPreference {
        self.policy.routing_preference()
    }

    /// Return the caller-provided relay connection tie-breaker.
    pub fn connection_weight(&self) -> u32 {
        self.policy.connection_weight()
    }

    pub(in crate::relay) fn retain_allowed(&mut self) {
        let source = self.policy.source();
        self.urls.retain(|relay| relay.allowed_for_source(source));
    }
}

// standardize the format (ie, trailing slashes)
fn canonicalize_url(url: String) -> String {
    match Url::parse(&url) {
        Ok(parsed_url) => parsed_url.to_string(),
        Err(_) => url, // If parsing fails, return the original URL.
    }
}

fn remote_advertised_url_parts_allowed(url: &Url) -> bool {
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return false;
    }

    url.path() == "/"
}

fn public_domain_host_allowed(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return false;
    }

    if domain == "localhost" || domain.ends_with(".localhost") {
        return false;
    }

    if domain == "local" || domain.ends_with(".local") {
        return false;
    }

    if domain == "onion" || domain.ends_with(".onion") {
        return false;
    }

    if !domain.contains('.') {
        return false;
    }

    domain.split('.').all(valid_dns_label)
}

fn valid_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== NormRelayUrl tests ====================

    #[test]
    fn norm_relay_url_creates_valid_url() {
        let url = NormRelayUrl::new("wss://relay.example.com");
        assert!(url.is_ok());
    }

    #[test]
    fn norm_relay_url_handles_trailing_slash() {
        let url1 = NormRelayUrl::new("wss://relay.example.com/").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        // Both should canonicalize to the same thing
        assert_eq!(url1.to_string(), url2.to_string());
    }

    #[test]
    fn norm_relay_url_rejects_invalid() {
        assert!(NormRelayUrl::new("not-a-url").is_err());
    }

    #[test]
    fn norm_relay_url_rejects_http() {
        // nostr relay URLs must be ws:// or wss://
        assert!(NormRelayUrl::new("http://relay.example.com").is_err());
    }

    #[test]
    fn remote_advertised_policy_rejects_local_private_and_unsupported_hosts() {
        for url in [
            "wss://localhost",
            "wss://127.0.0.1",
            "wss://8.8.8.8",
            "wss://10.0.0.1",
            "wss://172.16.0.1",
            "wss://192.168.0.1",
            "wss://169.254.0.1",
            "wss://224.0.0.1",
            "wss://100.64.0.1",
            "wss://192.0.2.1",
            "wss://198.51.100.1",
            "wss://203.0.113.1",
            "wss://198.18.0.1",
            "wss://240.0.0.1",
            "wss://[::1]",
            "wss://[2606:4700:4700::1111]",
            "wss://[fc00::1]",
            "wss://[fe80::1]",
            "wss://[ff02::1]",
            "wss://[2001:db8::1]",
            "wss://[100::1]",
            "wss://[64:ff9b::808:808]",
            "wss://[64:ff9b:1::1]",
            "wss://[2001::1]",
            "wss://[2001:2::1]",
            "wss://[2001:10::1]",
            "wss://[2001:20::1]",
            "wss://[2002::1]",
            "wss://[3fff::1]",
            "wss://[::192.168.0.1]",
            "wss://[::ffff:100.64.0.1]",
            "wss://[::ffff:224.0.0.1]",
            "wss://[::ffff:192.0.2.1]",
            "wss://relay.local",
            "wss://relay.onion",
            "wss://relay",
            "wss://bad_host.example.com",
            "wss://-bad.example.com",
            "wss://bad-.example.com",
        ] {
            if let Ok(relay) = NormRelayUrl::new(url) {
                assert!(
                    !relay.allowed_for_source(RelayUrlSource::RemoteAdvertised),
                    "{url} should not be allowed for remote-advertised relay URLs"
                );
            }
        }
    }

    #[test]
    fn remote_advertised_policy_rejects_obviously_invalid_endpoints() {
        for url in [
            "wss://user@relay.example.com",
            "wss://user:pass@relay.example.com",
            "wss://relay.example.com/#fragment",
            "wss://relay.example.com/path",
            "wss://relay.example.com/?q=relay",
            "wss://relay.example.com/path%0b",
            "wss://relay.example.com/path%20with-space",
            "wss://relay.example.com/?q=%7f",
            "wss://nostramsterdam.vpx.moewss//nostr.primz.org",
            "wss://rsslay.wss//relay.nostr.info%0b%20nostr.net",
        ] {
            let relay = NormRelayUrl::new(url).expect("syntactically valid relay URL");
            assert!(
                !relay.allowed_for_source(RelayUrlSource::RemoteAdvertised),
                "{url} should not be allowed for remote-advertised relay URLs"
            );
        }
    }

    #[test]
    fn remote_advertised_policy_allows_public_relay_hosts() {
        let url = "wss://relay.example.com";
        let relay = NormRelayUrl::new(url).expect("valid relay");
        assert!(
            relay.allowed_for_source(RelayUrlSource::RemoteAdvertised),
            "{url} should be allowed for remote-advertised relay URLs"
        );
    }

    #[test]
    fn explicit_policy_allows_local_relay_urls() {
        let relay = NormRelayUrl::new("ws://127.0.0.1:7777").expect("valid local relay");

        assert!(relay.allowed_for_source(RelayUrlSource::Explicit));
    }

    #[test]
    fn norm_relay_url_equality() {
        let url1 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        assert_eq!(url1, url2);
    }

    #[test]
    fn norm_relay_url_hash_consistency() {
        use std::collections::HashSet;

        let url1 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();

        let mut set = HashSet::new();
        set.insert(url1);
        assert!(set.contains(&url2));
    }

    // ==================== RelayUrlPkgs tests ====================

    #[test]
    fn relay_url_pkgs_new_sets_urls() {
        let mut urls = HashSet::new();
        urls.insert(NormRelayUrl::new("wss://relay1.example.com").unwrap());
        urls.insert(NormRelayUrl::new("wss://relay2.example.com").unwrap());

        let pkgs = RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        );
        assert_eq!(pkgs.urls.len(), 2);
        assert_eq!(pkgs.demand_priority(), RelayDemandPriority::Important);
        assert_eq!(
            pkgs.routing_preference(),
            RelayRoutingPreference::PreferDedicated
        );
    }

    #[test]
    fn relay_url_pkgs_builder_requires_explicit_priority_and_preference() {
        let mut urls = HashSet::new();
        urls.insert(NormRelayUrl::new("wss://relay-builder.example.com").unwrap());

        let pkgs = RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Critical,
                crate::relay::RelayRoutingPreference::RequireDedicated,
            ),
        );

        assert_eq!(pkgs.urls.len(), 1);
        assert_eq!(pkgs.demand_priority(), RelayDemandPriority::Critical);
        assert_eq!(
            pkgs.routing_preference(),
            RelayRoutingPreference::RequireDedicated
        );
    }

    #[test]
    fn relay_url_pkgs_iter() {
        let mut urls = HashSet::new();
        urls.insert(NormRelayUrl::new("wss://relay1.example.com").unwrap());

        let pkgs = RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        );
        assert_eq!(pkgs.iter().count(), 1);
    }

    // ==================== RelayREQId tests ====================

    #[test]
    fn relay_req_id_default_generates_uuid() {
        let id1 = RelayReqId::default();
        let id2 = RelayReqId::default();
        // Each default should generate a unique UUID
        assert_ne!(id1, id2);
    }

    // ==================== SubRequestId tests ====================

    #[test]
    fn sub_request_id_equality() {
        let id1 = OutboxSubId(42);
        let id2 = OutboxSubId(42);
        let id3 = OutboxSubId(43);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn sub_request_id_ordering() {
        let id1 = OutboxSubId(1);
        let id2 = OutboxSubId(2);

        assert!(id1 < id2);
    }
}
