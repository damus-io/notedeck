use crate::{
    filter::{self, HybridFilter, ValidKind},
    Error,
};
use nostrdb::{Filter, Note};

pub fn contacts_filter(pk: &[u8; 32]) -> Filter {
    Filter::new().authors([pk]).kinds([3]).limit(1).build()
}

/// Contact filters have an additional kind0 in the remote filter so it can fetch profiles as well
/// we don't need this in the local filter since we only care about the kind1 results
pub fn hybrid_contacts_filter(
    note: &Note,
    add_pk: Option<&[u8; 32]>,
    with_hashtags: bool,
) -> Result<HybridFilter, Error> {
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
