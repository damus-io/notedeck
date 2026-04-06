use crate::{
    filter::{self, HybridFilter, NdbQueryPackage, ValidKind},
    Error,
};
use nostrdb::{Filter, Note};

pub fn contacts_filter(pk: &[u8; 32]) -> Filter {
    Filter::new().authors([pk]).kinds([3]).limit(1).build()
}

/// Build a hybrid filter for the "last per pubkey" algo feed.
/// Local: all contacts (no sampling), kind 1. Remote: reservoir-sampled subset.
pub fn hybrid_last_per_pubkey_filter(
    note: &Note,
    notes_per_pk: u64,
) -> Result<HybridFilter, Error> {
    let kind = 1;
    let local_filters = filter::last_n_per_pubkey_from_tags(note, kind, notes_per_pk, None)?;
    let local = vec![NdbQueryPackage {
        filters: local_filters,
        kind: ValidKind::One,
    }];
    let remote = filter::last_n_per_pubkey_from_tags(note, kind, notes_per_pk, Some(15))?;
    Ok(HybridFilter::split(local, remote))
}

/// Contact filters have an additional kind0 in the remote filter so it can fetch profiles as well
/// we don't need this in the local filter since we only care about the kind1 results
pub fn hybrid_contacts_filter(
    note: &Note,
    add_pk: Option<&[u8; 32]>,
    with_hashtags: bool,
) -> Result<HybridFilter, Error> {
    tracing::debug!("entered hybrid_contacts_filter");
    let local = vec![
        filter::filter_from_tags(note, add_pk, with_hashtags)?
            .into_query_package(ValidKind::One, filter::default_limit()),
        filter::filter_from_tags(note, add_pk, with_hashtags)?
            .into_query_package(ValidKind::Six, filter::default_limit()),
        filter::filter_from_tags(note, add_pk, with_hashtags)?
            .into_query_package(ValidKind::Zero, filter::default_limit()),
    ];
    let remote = filter::filter_from_tags(note, add_pk, with_hashtags)?
        .into_filter(vec![1, 0], filter::default_remote_limit());

    Ok(HybridFilter::split(local, remote))
}
