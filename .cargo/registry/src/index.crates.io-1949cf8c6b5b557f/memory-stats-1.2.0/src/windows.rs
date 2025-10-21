use std::mem::MaybeUninit;

use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::MemoryStats;

pub fn memory_stats() -> Option<MemoryStats> {
    let mut maybe_pmc = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::uninit();
    match unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            maybe_pmc.as_mut_ptr(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as _,
        )
    } {
        // GetProcessMemoryInfo failed
        0 => None,
        _ => {
            // SAFETY: we have validated that GetProcessMemoryInfo succeeded
            let pmc = unsafe { maybe_pmc.assume_init() };
            Some(MemoryStats {
                physical_mem: pmc.WorkingSetSize,
                virtual_mem: pmc.PagefileUsage,
            })
        }
    }
}
