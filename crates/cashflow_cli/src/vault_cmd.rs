//! The commands that touch the nostr vault: `login`, `logout` and `settings`,
//! and the [`Session`] every vault command opens.
//!
//! Like `headway` and `notebook`, the CLI keeps its own nostrdb
//! (`<data-dir>/cashflow-cli`, or `--db`) and reconciles it with a relay
//! (`--relay`, `$CASHFLOW_RELAY`, else notedeck's embedded one) before it
//! reads, then publishes what it wrote. Only the vault's sealed kind-1081
//! envelopes move; nostrdb unwraps them locally once the vault root is
//! registered. An unreachable relay leaves the command working offline.

use cashflow::settings::{self, Setting, Settings};
use cashflow::{CashflowDir, RELAY_ENV, Vault};
use nostrdb::{Ndb, Transaction};
use nostrdb_net::relay::sync::{self, Relay};
use serde_json::json;

use crate::args::Run;

/// The CLI's nostrdb directory under the platform data dir.
const APP: &str = "cashflow-cli";

/// The secret holding the vault's signing key (`$CASHFLOW_NSEC`).
const NSEC_SECRET: &str = "nsec";

/// An open vault: the local cache, synced with the relay when one answered.
pub struct Session {
    /// The CLI's nostrdb, with the vault root registered.
    pub ndb: Ndb,
    /// The account's vault.
    pub vault: Vault,
    /// The relay, when it was reachable.
    relay: Option<Relay>,
}

impl Session {
    /// Open the vault: read the signing key, open the cache, register the
    /// vault root and reconcile the vault's envelopes with the relay.
    pub async fn open(dir: &CashflowDir, run: &Run) -> Result<Self, String> {
        let secret = signing_key(dir)?;
        let vault = Vault::new(&secret).ok_or("the stored nsec isn't a usable key")?;
        let db = run.db.as_ref().map(|path| path.to_string_lossy());
        let ndb = sync::open_ndb(db.as_deref(), APP).map_err(|e| e.to_string())?;
        vault.register(&ndb);

        let relay_url = relay_url(run);
        let relay = sync::connect_and_sync(
            &relay_url,
            &ndb,
            vault.team_pubkey(),
            &[nostrdb_net::sns::SNS_ENVELOPE_KIND],
            &vault.sync_filter(),
            // Envelopes are immutable, one per edit: nothing is latest-wins
            // on the wire, only inside them.
            &|_| false,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(Self { ndb, vault, relay })
    }

    /// Send `frames` to the relay, if one is connected.
    pub async fn publish(&mut self, frames: &[String]) -> Result<(), String> {
        sync::publish(&mut self.relay, frames)
            .await
            .map_err(|e| e.to_string())
    }

    /// What to add to a write's output when it never reached a relay.
    pub fn offline_note(&self) -> &'static str {
        if self.relay.is_some() {
            ""
        } else {
            " (offline: stored locally, not published)"
        }
    }

    /// [`Self::open`], or `None` when no signing key is stored, for a
    /// command that works without the vault.
    pub async fn open_if_logged_in(dir: &CashflowDir, run: &Run) -> Result<Option<Self>, String> {
        if dir
            .secret(NSEC_SECRET)
            .map_err(|e| e.to_string())?
            .is_none()
        {
            return Ok(None);
        }
        Self::open(dir, run).await.map(Some)
    }

    /// A read transaction on the cache.
    pub fn txn(&self) -> Result<Transaction, String> {
        Transaction::new(&self.ndb).map_err(|e| e.to_string())
    }
}

/// The agentium session this run is part of: `$AGENTIUM_SESSION` when it is
/// an `agentium:<word-id>` ref, so a malformed value is never recorded on a
/// rule or edit.
pub fn session_ref() -> Option<String> {
    let value = std::env::var("AGENTIUM_SESSION").ok()?;
    match wordid::WordIdRef::parse_uri(value.trim(), "agentium")? {
        wordid::WordIdRef::Bare { words } => Some(format!("agentium:{words}")),
        wordid::WordIdRef::Scoped { .. } => None,
    }
}

/// The relay this run syncs with: `--relay`, else `$CASHFLOW_RELAY`, else
/// notedeck's embedded relay.
fn relay_url(run: &Run) -> String {
    run.relay
        .clone()
        .or_else(|| std::env::var(RELAY_ENV).ok())
        .unwrap_or_else(|| sync::DEFAULT_RELAY.to_owned())
}

/// The vault's signing key: the secret `nsec`.
fn signing_key(dir: &CashflowDir) -> Result<[u8; 32], String> {
    let nsec = dir
        .secret(NSEC_SECRET)
        .map_err(|e| e.to_string())?
        .ok_or("no signing key: run `cashflow login`, or set $CASHFLOW_NSEC")?;
    let (secret, _) = sync::parse_nsec(nsec.expose().trim()).map_err(|e| e.to_string())?;
    Ok(secret)
}

/// The account the stored signing key is for, without opening the vault.
pub fn account_pubkey(dir: &CashflowDir) -> Result<nostrdb_net::Pubkey, String> {
    let secret = signing_key(dir)?;
    let keypair = nostrdb_net::FullKeypair::from_secret_bytes(&secret)
        .ok_or("the stored nsec isn't a usable key")?;
    Ok(keypair.pubkey)
}

/// `cashflow login [<nsec>]`: validate the key, then store it as the secret
/// `nsec`. With no argument the key is the first line of stdin.
pub fn login(dir: &CashflowDir, nsec: Option<String>) -> Result<(), String> {
    let nsec = match nsec {
        Some(nsec) => nsec,
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| format!("reading the nsec from stdin: {e}"))?;
            line
        }
    };
    let nsec = nsec.trim();
    let (_, pubkey) = sync::parse_nsec(nsec).map_err(|e| e.to_string())?;
    let path = dir
        .write_secret(NSEC_SECRET, nsec)
        .map_err(|e| e.to_string())?;
    println!(
        "stored signing key for {} in {}",
        pubkey.hex(),
        path.display()
    );
    Ok(())
}

/// `cashflow logout`: delete the stored key.
pub fn logout(dir: &CashflowDir) -> Result<(), String> {
    match dir.remove_secret(NSEC_SECRET).map_err(|e| e.to_string())? {
        Some(path) => println!("removed stored signing key at {}", path.display()),
        None => println!("no stored signing key"),
    }
    Ok(())
}

/// The setting called `name`, or an error listing the ones there are.
fn setting_named(name: &str) -> Result<Setting, String> {
    Setting::from_name(name).ok_or_else(|| {
        let known: Vec<&str> = Setting::ALL.iter().map(|s| s.name()).collect();
        format!("no setting named '{name}' (settings: {})", known.join(", "))
    })
}

/// `cashflow settings [show] [--json]`.
pub fn show(session: &Session, json: bool) -> Result<(), String> {
    let txn = session.txn()?;
    let settings = Settings::load(&session.ndb, &txn, session.vault.author());
    if json {
        let rows: Vec<_> = Setting::ALL
            .iter()
            .map(|&s| {
                json!({
                    "name": s.name(),
                    "value": settings.effective(s),
                    "default": settings.value(s).is_none(),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    for setting in Setting::ALL {
        let marker = if settings.value(setting).is_none() {
            "  (default)"
        } else {
            ""
        };
        println!(
            "{:<10}{}{marker}",
            setting.name(),
            settings.effective(setting)
        );
    }
    Ok(())
}

/// `cashflow settings get <name>`: the value alone, default included.
pub fn get(session: &Session, name: &str) -> Result<(), String> {
    let setting = setting_named(name)?;
    let txn = session.txn()?;
    let settings = Settings::load(&session.ndb, &txn, session.vault.author());
    println!("{}", settings.effective(setting));
    Ok(())
}

/// `cashflow settings set <name> <value>`: write it, then publish it.
pub async fn set(session: &mut Session, name: &str, value: &str) -> Result<(), String> {
    let setting = setting_named(name)?;
    let written =
        settings::set(&session.vault, &session.ndb, setting, value).map_err(|e| e.to_string())?;
    session.publish(&[written.frame]).await?;
    println!("{} = {value}{}", setting.name(), session.offline_note());
    Ok(())
}
