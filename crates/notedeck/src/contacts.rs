use crate::{
    Error,
    filter::{self, HybridFilter},
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
    let local = filter::filter_from_tags(note, add_pk, with_hashtags)?
        .into_filter([1], filter::default_limit());
    let remote = filter::filter_from_tags(note, add_pk, with_hashtags)?
        .into_filter([1, 0], filter::default_remote_limit());

    Ok(HybridFilter::split(local, remote))
}
