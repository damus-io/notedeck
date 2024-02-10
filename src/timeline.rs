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

pub fn merge_sorted_vecs<T: Ord + Copy>(vec1: &Vec<T>, vec2: &Vec<T>) -> Vec<T> {
    let mut merged = Vec::with_capacity(vec1.len() + vec2.len());
    let mut i = 0;
    let mut j = 0;

    while i < vec1.len() && j < vec2.len() {
        if vec1[i] <= vec2[j] {
            merged.push(vec1[i]);
            i += 1;
        } else {
            merged.push(vec2[j]);
            j += 1;
        }
    }

    // Append any remaining elements from either vector
    if i < vec1.len() {
        merged.extend_from_slice(&vec1[i..]);
    }
    if j < vec2.len() {
        merged.extend_from_slice(&vec2[j..]);
    }

    merged
}
