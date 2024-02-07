pub fn binary_search<T: Ord>(a: &[T], item: &T) -> usize {
    let mut low = 0;
    let mut high = a.len();

    while low < high {
        let mid = low + (high - low) / 2;
        if item <= &a[mid] {
            high = mid;
        } else {
            low = mid + 1;
        }
    }

    low
}

pub fn binary_insertion_sort<T: Ord>(vec: &mut Vec<T>) {
    for i in 1..vec.len() {
        let val = vec.remove(i);
        let pos = binary_search(&vec[0..i], &val);
        vec.insert(pos, val);
    }
}
