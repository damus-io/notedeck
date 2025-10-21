//! [Issue 13: vec1! macro doesn't accept trailing comma](https://github.com/rustonaut/vec1/issues/13)
//!

#[macro_use]
extern crate vec1;

#[test]
fn allow_trailing_comma_in_vec_macro() {
    let _ = vec1![1u8,];
    let _ = vec1![1u8, 2u8,];
    let _ = vec1![1u8, 2u8, 3u8,];
}

#[test]
fn allow_no_trailing_comma_in_vec_macro() {
    let _ = vec1![1u8];
    let _ = vec1![1u8, 2u8];
    let _ = vec1![1u8, 2u8, 3u8];
}
