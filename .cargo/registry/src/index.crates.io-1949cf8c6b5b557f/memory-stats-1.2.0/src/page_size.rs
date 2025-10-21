use std::sync::atomic::{AtomicUsize, Ordering};

pub static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Grabs the value of the SC_PAGESIZE if needed.
pub fn load_page_size() -> Option<()> {
    if PAGE_SIZE.load(Ordering::Relaxed) == 0 {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size == -1 {
            // sysconf returned error
            return None;
        } else {
            PAGE_SIZE.store(page_size as usize, Ordering::Relaxed);
        }
    }
    Some(())
}
