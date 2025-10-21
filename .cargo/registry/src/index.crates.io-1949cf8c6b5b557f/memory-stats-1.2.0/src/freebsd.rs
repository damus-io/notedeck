use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::MemoryStats;

#[path = "page_size.rs"]
mod page_size;

#[link(name = "util")]
extern "C" {
    fn kinfo_getproc(pid: libc::pid_t) -> *mut libc::kinfo_proc;
}

pub fn memory_stats() -> Option<MemoryStats> {
    struct FreeLater<T> {
        p: *mut T,
    }

    impl<T> Drop for FreeLater<T> {
        fn drop(&mut self) {
            if !self.p.is_null() {
                unsafe { libc::free(self.p as *mut c_void) };
            }
        }
    }

    page_size::load_page_size()?;

    let info_ptr = FreeLater {
        p: unsafe { kinfo_getproc(libc::getpid()) },
    };

    if info_ptr.p.is_null() {
        None
    } else {
        // SAFETY: ptr is not null
        let info = unsafe { info_ptr.p.read() };
        let page_size = page_size::PAGE_SIZE.load(Ordering::Relaxed);
        Some(MemoryStats {
            physical_mem: (info.ki_rssize as usize) * page_size,
            virtual_mem: info.ki_size,
        })
    }
}
