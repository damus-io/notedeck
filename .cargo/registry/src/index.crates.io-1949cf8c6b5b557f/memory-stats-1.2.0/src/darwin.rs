use std::mem::MaybeUninit;

use libc::{
    mach_msg_type_number_t, mach_task_basic_info_data_t, mach_task_self, task_info, task_info_t, KERN_SUCCESS,
    MACH_TASK_BASIC_INFO, MACH_TASK_BASIC_INFO_COUNT,
};

use crate::MemoryStats;

pub fn memory_stats() -> Option<MemoryStats> {
    let mut maybe_taskinfo = MaybeUninit::<mach_task_basic_info_data_t>::uninit();
    let mut count = MACH_TASK_BASIC_INFO_COUNT;
    match unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            maybe_taskinfo.as_mut_ptr() as task_info_t,
            &mut count as *mut mach_msg_type_number_t,
        )
    } {
        KERN_SUCCESS => {
            // SAFETY: we have validated that task_info succeeded
            let taskinfo = unsafe { maybe_taskinfo.assume_init() };
            Some(MemoryStats {
                physical_mem: taskinfo.resident_size as usize,
                virtual_mem: taskinfo.virtual_size as usize,
            })
        }
        // task_info failed
        _ => None,
    }
}
