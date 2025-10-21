
# Change Log

## Version 1.12.1 (25.05.2024)

- Reduced minimal rust version to 1.71.1

## Version 1.12.0 (27.03.2024)

- Added `len_nonzero`.

## Version 1.11.1 (23.03.2024)

- Fix `package.rust-version` in `Cargo.toml`.

## Version 1.11.0 (23.03.2024)

- Increased minimal rust version to 1.74.
- Relax lifetime constraints on `{try_,}mapped_{ref,mut}`.
- Added new proxy functions for `Vec`
  - `try_reserve`
  - `try_reserve_exact`
  - `shrink_to`
  - `spare_capacity_mut`
  - `extend_from_within`
  - `retain_mut`
- Added missing proxy trait impls
  - `impl<T, const N: usize> TryFrom<Vec1<T>> for Box<[T; N]>`
  - `impl<T, const N: usize> TryFrom<&[T; N]> for Vec1<T> where T: Clone`
  - `impl<T, const N: usize> TryFrom<&mut [T; N]> for Vec1<T> where T: Clone`
- Removed no longer needed import/impl workaround.
- Added multiple `_nonzero` implementations
  - `truncate_nonzero`
  - `resize_with_nonzero`
  - `resize_nonzero`

## Version 1.10.1 (21.10.2022)

- Improved documentation by using `doc_auto_cfg` on docs.rs.

## Version 1.10.0 (21.10.2022)

- Increased minimal rust version to 1.57.
- Added a length>0 aware `reduce`, `reduce_ref`, `reduce_mut`.
- Added `smallvec1_inline!`.
- Added `SmallVec1::from_array_const()`.

## Version 1.9.0 (16.10.2022)

- Increased minimal rust version to 1.56.
- Added missing LICENSE-MIT,LICENSE-APACHE files. Licensing did not change.
- Added `from_vec_push` and `from_vec_insert` constructors.
- Use edition 2021.
- Impl `TryFrom` for `[T; N]` for `Vec1<T>`/`SmallVec1<T>` using const generic.

## Version 1.8.0 (21.04.2021)

- minimal rust version is now 1.48
- updated documentation
- more tests
- deprecated the `try_` prefix usage as it created ambiguities with
  other potential `try_` versions (like try and don't panic if out of
  bounds or try and don't panic if allocation fails).
- some missing methods and trait implementations (e.g. `drain)
- fixed bug in `Vec1.splice()` which caused the code to return
  a `Size0Error` in a very specific edge case where it should
  have panicked due to a out of bounds range like `Vec.splice()`
  does.

## Version 1.7.0 (11.03.2021)

- minimal rust version is now 1.47
- support for `SmallVec1` backed by the `smallvec` crate (v>=1.6.1)
- added `no_std` support (making `std` a default feature)
- converted various `Into`/`TryInto` impls into `From`/`TryFrom` impls.
- changes in the documentation for various reasons, some functions
  have now less good documentation as they are automatically implemented
  for both `Vec1`, and `smallvec-v1::SmallVec1`.

## Version 1.6.0 (11.08.2020)

- Added the `split_off_first` and `split_off_last` methods.

## Version 1.5.1 (01.07.2020)

- Updated project to `edition="2018"` (not that this is
  a purely internal change and doesn't affect the API
  interface or minimal supported rustc version)
- Added [CONTRIBUTORS.md](./CONTRIBUTORS.md)
- Updated [README.md](./README.md)

## Version 1.5.0 (21.05.2020)

- minimal rust version is now 1.34
- `TryFrom` is no longer feature gated
- `vec1![]` now allows trailing `,` in all cases
- `Size0Error` now no longer has a custom
  `std::error::Error::description()` implementation.
- fixed various clippy::pedantic warnings
- updated `Cargo.toml`
- `cargo fmt`

## Version 1.4.0 (26.03.2019)

New trait impl:
- impl Default for Vec1<T> where T: Default

## Version 1.3.0 (21.03.2019)

New manual proxy methods:
- splice
- to_asci_lowercase
- to_ascii_uppercase

New Into impl for following types:
- Rc<[T]>
- Arc<[T]>
- Box<[T]>
- VecDeque<T>

### Unstable/Nightly features

New TryFrom impl for following types:
- Box<[T]>
- BinaryHeap<T>
- VecDeque<T>
- String
- &str
- &[T] where T: Clone
- &mut [T] where T: Clone

## Version 1.2.0 (20.03.2019)

- Added new `try_from_vec` which returns a `Result<Vec1<T>, Size0Error>`.
- Deprecated `from_vec` as it doesn't return a error type as error.

### Unstable/Nightly features

- New `unstable-nightly-try-from-impl` feature which adds a `TryFrom<Vec<T>>` implementation.


## Version 1.1.0

- Addead a `serde` feature implementing `Serialize`/`Deserialize`.
