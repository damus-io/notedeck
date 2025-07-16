use nostrdb::Filter;

pub fn contacts_filter(pk: &[u8; 32]) -> Filter {
    Filter::new().authors([pk]).kinds([3]).limit(1).build()
}
