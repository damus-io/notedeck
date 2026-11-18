//! Where a guest may connect: a short list of hostnames, HTTPS only, with
//! everything else denied, DNS included.
//!
//! ```text
//!   guest ── DNS "github.com"  ─▶ gateway resolver ── allowlisted name? ─▶ upstream, answer cached
//!         ── DNS "evil.example" ▶ gateway resolver ── no ───────────────▶ NXDOMAIN, at once
//!         ── TCP 140.82.x.x:443 ▶ policy: an IP cached for an allowlisted name, port 443? ─▶ allow
//!         ── TCP anything else  ▶ default deny
//! ```
//!
//! # What is on the list, and what isn't
//!
//! The model conversation runs on the host (headway:dave/liar-imitate-print),
//! so the guest never calls a model API and `api.anthropic.com` is not on
//! the list. What's left is what the agent's own tools legitimately reach,
//! [`DEFAULT_HOSTS`]: git remotes on `github.com`, and the Rust and npm
//! registries. A project that needs more names adds them with
//! [`Egress::allow`]. A list too narrow to install a dependency makes the
//! agent useless, and one too wide is an exfiltration path, so the default
//! is the floor a coding session needs and nothing above it.
//!
//! Only port 443 is opened. git over ssh would need a key the guest doesn't
//! have, so a port-22 rule would grant nothing.
//!
//! The host's LAN, its loopback and the cloud metadata address are all
//! denied, because nothing allows them. A guest that has to reach notedeck
//! does so over vsock ([`crate::ipc`]), which never touches the network and
//! so needs no rule here.
//!
//! # github.com is also the way out
//!
//! The guest has the repository. `github.com` on the list plus any
//! credential is enough to push it somewhere else. **What keeps git
//! remotes read-only today is that the guest holds no credentials** (no
//! `~/.ssh`, no credential helper, no token in its env; see
//! [`crate::mounts`] for the push that lands through a readonly git dir),
//! not this list. Giving the guest a credential later silently changes
//! that, and whoever does it owns the change.
//!
//! # DNS: the footgun, and why this policy doesn't hit it
//!
//! The spike (headway:dave/solid-stay-cup) found that `default_deny()` plus
//! `egress(allow_public())` breaks DNS: the guest's resolver is the
//! sandbox gateway, which isn't a "public" address, and an IP-based rule
//! can't match a lookup that has no address yet. In the spike it surfaced
//! as `npm error network ... behind a proxy` after a 70-second hang.
//!
//! microsandbox 0.6.13 evaluates every DNS query against the egress policy
//! by its **name**: a `Domain` rule matches the name itself, whatever ports
//! it opens for the later connection, while IP and group rules never match
//! a query. So the domain rules this module builds are also its DNS
//! allowlist, and the guest resolves exactly the names it may connect to.
//! There is deliberately no blanket "DNS to the gateway" rule: that would
//! answer every name through the host's resolver, a tunnel out for
//! anything that can be spelled as a hostname. A denied name comes back as
//! NXDOMAIN at once (its forwarder answers NXDOMAIN rather than REFUSED,
//! which stub resolvers retry until they time out), so a tool that needs a name off the list fails fast, with a name
//! in its error.
//!
//! That is a property of microsandbox's DNS forwarder, not of this crate,
//! and microsandbox is beta and moving. So it is checked twice:
//! `tests::every_host_resolves_and_nothing_else` asks the built policy
//! itself, and [`check_dns`] asks a booted guest, so a resolver that is
//! blocked or unreachable is a clear error at boot rather than a mystery
//! hang inside the first `cargo build`.

use std::time::Duration;

use crate::SandboxError;

/// The hostnames a coding session reaches unless its sandbox says
/// otherwise: git remotes and the Rust and npm registries. The first is
/// what [`check_dns`] resolves at boot.
pub const DEFAULT_HOSTS: &[&str] = &[
    "github.com",
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "registry.npmjs.org",
];

/// The one port an allowlisted host is reachable on.
pub const HTTPS: u16 = 443;

/// How long [`check_dns`] waits for the guest to resolve a name. A working
/// resolver answers in milliseconds, and the footgun it guards against
/// hung for 70 seconds.
pub const DNS_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The exit status [`dns_probe_script`] uses when the image has neither
/// `getent` nor `nslookup`: the shell's own "command not found".
pub const NO_RESOLVER_TOOL: i32 = 127;

/// A guest's egress allowlist: the hostnames it may connect to on
/// [`HTTPS`], and resolve. Everything else is denied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    hosts: Vec<String>,
}

impl Default for Egress {
    /// [`Egress::coding`].
    fn default() -> Self {
        Self::coding()
    }
}

impl Egress {
    /// [`DEFAULT_HOSTS`]: what a coding session needs.
    pub fn coding() -> Self {
        Self {
            hosts: DEFAULT_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    /// No egress at all: the guest resolves nothing and connects nowhere.
    pub fn offline() -> Self {
        Self { hosts: Vec::new() }
    }

    /// Also allow `host`, an exact name (`pypi.org`, not `*.pypi.org`).
    /// Names are lowercased; one already on the list is not repeated.
    /// Fails on anything but letters, digits, `-` and `.`, so a name can
    /// never carry a wildcard, a port or shell syntax into the policy or
    /// the boot check.
    pub fn allow(mut self, host: &str) -> Result<Self, SandboxError> {
        if !is_hostname(host) {
            return Err(SandboxError::BadHost(host.to_owned()));
        }
        let host = host.to_ascii_lowercase();
        if !self.hosts.contains(&host) {
            self.hosts.push(host);
        }
        Ok(self)
    }

    /// The allowed hostnames, in the order they were added.
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// The name [`check_dns`] should resolve: the first on the list, or
    /// none for an offline sandbox, which has no DNS to check.
    pub fn probe_host(&self) -> Option<&str> {
        self.hosts.first().map(String::as_str)
    }

    /// The microsandbox policy: egress denied by default, then one rule per
    /// host allowing TCP to [`HTTPS`] on the addresses that name resolved
    /// to. Ingress is left at microsandbox's default; a guest is reachable
    /// only through ports published on purpose, and we publish none.
    #[cfg(microvm)]
    pub fn policy(&self) -> Result<microsandbox::NetworkPolicy, SandboxError> {
        use microsandbox::NetworkAction;

        microsandbox::NetworkPolicy::builder()
            .default_egress(NetworkAction::Deny)
            .egress(|rule| {
                for host in &self.hosts {
                    rule.tcp().port(HTTPS).allow().domain(host.as_str());
                }
                rule
            })
            .build()
            .map_err(SandboxError::Policy)
    }

    /// Put this allowlist on `builder` as the sandbox's network policy.
    #[cfg(microvm)]
    pub fn apply(
        &self,
        builder: microsandbox::sandbox::SandboxBuilder,
    ) -> Result<microsandbox::sandbox::SandboxBuilder, SandboxError> {
        let policy = self.policy()?;
        Ok(builder.network(|net| net.policy(policy)))
    }
}

/// What [`check_dns`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsCheck {
    /// The guest resolved the name.
    Resolved {
        /// How long the lookup took, exec round trip included.
        elapsed: Duration,
    },
    /// The image has neither `getent` nor `nslookup`, so nothing was
    /// checked. Every glibc image has `getent` and every alpine image has
    /// busybox `nslookup`, so this is a project image built from scratch.
    NoResolverTool,
}

/// The shell script that resolves `host` in the guest: `getent` where the
/// image has it (glibc, and musl-utils on alpine), else `nslookup`
/// (busybox), else exit [`NO_RESOLVER_TOOL`]. `host` must have passed
/// [`is_hostname`]; [`Egress::allow`] guarantees that for every name on a
/// list.
pub fn dns_probe_script(host: &str) -> String {
    debug_assert!(is_hostname(host), "unchecked host {host:?}");
    format!(
        "if command -v getent >/dev/null 2>&1; then getent hosts {host}; \
         elif command -v nslookup >/dev/null 2>&1; then nslookup {host}; \
         else exit {NO_RESOLVER_TOOL}; fi"
    )
}

/// Whether `host` is a plain DNS name: dot-separated labels of ASCII
/// letters, digits and inner hyphens, each 1 to 63 bytes, 253 in all.
pub fn is_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Resolve `host` inside `sandbox`, within [`DNS_CHECK_TIMEOUT`]: the boot
/// self-check that DNS works under the sandbox's egress policy. Run it
/// right after boot with [`Egress::probe_host`], before the agent starts,
/// so a blocked or unreachable resolver is one clear error instead of a
/// tool that hangs and then blames a proxy.
#[cfg(microvm)]
pub async fn check_dns(
    sandbox: &microsandbox::Sandbox,
    host: &str,
) -> Result<DnsCheck, SandboxError> {
    if !is_hostname(host) {
        return Err(SandboxError::BadHost(host.to_owned()));
    }
    let started = std::time::Instant::now();
    let ran = tokio::time::timeout(DNS_CHECK_TIMEOUT, sandbox.shell(dns_probe_script(host))).await;
    let Ok(output) = ran else {
        return Err(SandboxError::DnsUnresolved {
            host: host.to_owned(),
            detail: format!("no answer in {}s", DNS_CHECK_TIMEOUT.as_secs()),
        });
    };
    let output = output?;
    let status = output.status();
    if status.code == NO_RESOLVER_TOOL {
        return Ok(DnsCheck::NoResolverTool);
    }
    if !status.success {
        let stderr = String::from_utf8_lossy(output.stderr_bytes());
        let stdout = String::from_utf8_lossy(output.stdout_bytes());
        let said = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(SandboxError::DnsUnresolved {
            host: host.to_owned(),
            detail: format!("exit {}: {}", status.code, said.trim()),
        });
    }
    Ok(DnsCheck::Resolved {
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_coding_floor_without_a_model_api() {
        let egress = Egress::default();
        assert_eq!(egress.hosts(), DEFAULT_HOSTS);
        assert!(!egress.hosts().iter().any(|h| h.contains("anthropic")));
        assert_eq!(egress.probe_host(), Some("github.com"));
    }

    #[test]
    fn offline_has_nothing_to_resolve() {
        let egress = Egress::offline();
        assert!(egress.hosts().is_empty());
        assert_eq!(egress.probe_host(), None);
    }

    #[test]
    fn a_project_adds_a_host_once_lowercased() {
        let egress = Egress::coding()
            .allow("PyPI.org")
            .unwrap()
            .allow("pypi.org")
            .unwrap();
        assert_eq!(egress.hosts().last().unwrap(), "pypi.org");
        assert_eq!(egress.hosts().len(), DEFAULT_HOSTS.len() + 1);
    }

    /// Nothing but a bare name gets into the policy or the probe script.
    #[test]
    fn a_host_that_is_not_a_bare_name_is_refused() {
        for bad in [
            "",
            "*.pypi.org",
            "pypi.org:443",
            "a..b",
            "-x.org",
            "x-.org",
            "x.org; rm -rf /",
            "$(id).org",
            "https://pypi.org",
            &"a".repeat(64),
        ] {
            assert!(
                matches!(Egress::offline().allow(bad), Err(SandboxError::BadHost(_))),
                "{bad:?} was accepted"
            );
        }
        for good in DEFAULT_HOSTS {
            assert!(is_hostname(good), "{good}");
        }
    }

    #[test]
    fn the_probe_tries_getent_then_nslookup_then_says_neither() {
        let script = dns_probe_script("github.com");
        let getent = script.find("getent hosts github.com").unwrap();
        let nslookup = script.find("nslookup github.com").unwrap();
        assert!(getent < nslookup);
        assert!(script.ends_with(&format!("exit {NO_RESOLVER_TOOL}; fi")));
    }

    /// The footgun, asked of the policy we actually build: every
    /// allowlisted name resolves over the gateway's UDP and TCP DNS, and
    /// any other name, the gateway's own DNS port included, is denied.
    /// This is microsandbox's forwarder logic (`decide_dns_action`), so a
    /// microsandbox upgrade that changes how DNS meets the policy fails
    /// here before it fails in a guest.
    #[cfg(microvm)]
    #[test]
    fn every_host_resolves_and_nothing_else() {
        use microsandbox::NetworkAction;
        use microsandbox_network::policy::{DomainName, Protocol};

        let policy = Egress::coding().policy().unwrap();
        let dns = |name: &str, protocol| {
            let name: DomainName = name.parse().unwrap();
            policy.evaluate_dns_query(&name, protocol, 53)
        };
        for host in DEFAULT_HOSTS {
            assert_eq!(dns(host, Protocol::Udp), NetworkAction::Allow, "{host}/udp");
            assert_eq!(dns(host, Protocol::Tcp), NetworkAction::Allow, "{host}/tcp");
        }
        for other in [
            "api.anthropic.com",
            "example.com",
            "evil.github.com.example",
        ] {
            assert_eq!(dns(other, Protocol::Udp), NetworkAction::Deny, "{other}");
        }
        assert_eq!(
            policy.evaluate_dns_query_without_name(Protocol::Udp, 53),
            NetworkAction::Deny
        );
    }

    /// The spike's broken shape, kept as a fact about microsandbox: an
    /// IP-group allow under default deny grants no DNS at all.
    #[cfg(microvm)]
    #[test]
    fn allow_public_under_default_deny_resolves_nothing() {
        use microsandbox::NetworkAction;
        use microsandbox_network::policy::{DomainName, Protocol};

        let footgun = microsandbox::NetworkPolicy::builder()
            .default_egress(NetworkAction::Deny)
            .egress(|rule| rule.allow_public())
            .build()
            .unwrap();
        let name: DomainName = "github.com".parse().unwrap();
        assert_eq!(
            footgun.evaluate_dns_query(&name, Protocol::Udp, 53),
            NetworkAction::Deny
        );
    }

    /// Offline is a policy with no rules: deny everything, DNS included.
    #[cfg(microvm)]
    #[test]
    fn offline_denies_every_lookup() {
        use microsandbox::NetworkAction;
        use microsandbox_network::policy::{DomainName, Protocol};

        let policy = Egress::offline().policy().unwrap();
        assert!(policy.rules.is_empty());
        let name: DomainName = "github.com".parse().unwrap();
        assert_eq!(
            policy.evaluate_dns_query(&name, Protocol::Udp, 53),
            NetworkAction::Deny
        );
    }

    /// One rule per host, TCP 443 only.
    #[cfg(microvm)]
    #[test]
    fn each_host_gets_tcp_443_and_nothing_wider() {
        use microsandbox::NetworkAction;
        use microsandbox_network::policy::{Destination, PortRange, Protocol};

        let policy = Egress::coding().policy().unwrap();
        assert_eq!(policy.default_egress, NetworkAction::Deny);
        assert_eq!(policy.rules.len(), DEFAULT_HOSTS.len());
        for (rule, host) in policy.rules.iter().zip(DEFAULT_HOSTS) {
            let Destination::Domain(name) = &rule.destination else {
                panic!("{host}: not a domain rule: {:?}", rule.destination);
            };
            assert_eq!(name.as_str(), *host);
            assert_eq!(rule.protocols, [Protocol::Tcp]);
            assert_eq!(rule.ports, [PortRange::single(HTTPS)]);
            assert_eq!(rule.action, NetworkAction::Allow);
        }
    }
}
