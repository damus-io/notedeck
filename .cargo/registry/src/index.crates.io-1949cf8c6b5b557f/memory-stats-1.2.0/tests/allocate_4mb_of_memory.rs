use std::time::Instant;

use memory_stats::memory_stats;

/// Tests if the reported memory increases
/// reasonably if 4mb of memory is allocated
/// and then used.
///
/// Note that this test will likely fail if
/// smaps is not avaliable on Linux, since
/// the physical memory metric is inaccurate.
#[test]
fn allocate_4mb_of_memory() {
    const FOUR_MB: usize = 4_000_000;
    let initial = memory_stats().unwrap();
    println!("Initial memory usage: {:?}", initial);

    // allocate, but do not use, about 4 kb of memory
    let mut memories = Vec::<u8>::with_capacity(FOUR_MB);
    let after_alloc = memory_stats().unwrap();
    println!("Memory usage after allocating 4mb: {:?}", after_alloc);
    assert!(after_alloc.virtual_mem >= initial.virtual_mem + FOUR_MB);

    // fill the above with non-determinstic data
    let start = Instant::now();
    for _ in 0..FOUR_MB {
        memories.push((Instant::now().duration_since(start).as_nanos() % u8::MAX as u128) as u8);
    }
    let after_fill = memory_stats().unwrap();
    println!("Memory usage after filling 4mb: {:?}", after_fill);
    assert!(after_fill.physical_mem >= initial.physical_mem + FOUR_MB);
}
