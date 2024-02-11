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
