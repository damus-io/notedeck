//! A alternative `Vec1` implementation backed by an `SmallVec1`.
//!
//! # Construction Macro
//!
//! A macro similar to `vec!` or `vec1!` does exist and is
//! re-exported in this module as `smallvec1`.
//!
//! Due to limitations in rust we can't properly document it
//! directly without either giving it strange names or ending
//! up with name collisions once we support smallvec v2 in the
//! future (without introducing a braking change).
//!
//! ## Example
//!
//! ```rust
//! use vec1::smallvec_v1::{smallvec1, SmallVec1};
//! let v: SmallVec1<[u8; 4]> = smallvec1![1u8, 2];
//! assert_eq!(&*v, &*vec![1u8,2]);
//! ```

use crate::Size0Error;

#[cfg(feature = "smallvec-v1-write")]
use std::io;

use alloc::boxed::Box;
use alloc::vec::Vec;
use smallvec::*;
use smallvec_v1_ as smallvec;

pub use crate::__smallvec1_inline_macro_v1 as smallvec1_inline;
pub use crate::__smallvec1_macro_v1 as smallvec1;

use smallvec::Drain;

#[doc(hidden)]
#[macro_export]
macro_rules! __smallvec1_macro_v1 {
    () => (
        compile_error!("SmallVec1 needs at least 1 element")
    );
    ($first:expr $(, $item:expr)* , ) => (
        $crate::smallvec_v1::smallvec1!($first $(, $item)*)
    );
    ($first:expr $(, $item:expr)* ) => ({
        let smallvec = $crate::smallvec_v1_::smallvec!($first $(, $item)*);
        $crate::smallvec_v1::SmallVec1::try_from_smallvec(smallvec).unwrap()
    });
}

#[doc(hidden)]
#[macro_export]
macro_rules! __smallvec1_inline_macro_v1 {
    () => (
        compile_error!("SmallVec1 needs at least 1 element")
    );
    ($first:expr $(, $item:expr)* , ) => (
        $crate::smallvec_v1::smallvec1_inline!($first $(, $item)*)
    );
    ($first:expr $(, $item:expr)* ) => ({
        $crate::smallvec_v1::SmallVec1::from_array_const([$first $(, $item)*])
    });
}

shared_impl! {
    base_bounds_macro = A: Array,
    item_ty_macro = A::Item,

    /// `smallvec::SmallVec` wrapper which guarantees to have at least 1 element.
    ///
    /// `SmallVec1<T>` dereferences to `&[T]` and `&mut [T]` as functionality
    /// exposed through this can not change the length.
    ///
    /// Methods of `SmallVec` which can be called without reducing the length
    /// (e.g. `capacity()`, `reserve()`) are exposed through wrappers
    /// with the same function signature.
    ///
    /// Methods of `SmallVec` which could reduce the length to 0
    /// are implemented with a `try_` prefix returning a `Result`.
    /// (e.g. `try_pop(&self)`, `try_truncate()`, etc.).
    ///
    /// Methods with returned `Option<T>` with `None` if the length was 0
    /// (and do not reduce the length) now return T. (e.g. `first`,
    /// `last`, `first_mut`, etc.).
    ///
    /// All stable traits and methods implemented on `SmallVec<T>` _should_ also
    /// be implemented on `SmallVec1<T>` (except if they make no sense to implement
    /// due to the len 1 guarantee). Be aware implementations may lack behind a bit,
    /// fell free to open a issue/make a PR, but please search closed and open
    /// issues for duplicates first.
    pub struct SmallVec1<A>(SmallVec<A>);
}

impl<A> SmallVec1<A>
where
    A: Array,
{
    /// Tries to create a new instance from a instance of the wrapped type.
    ///
    /// # Errors
    ///
    /// This will fail if the input is empty.
    /// The returned error is a `Size0Error` instance, as
    /// such this means the _input vector will be dropped if
    /// it's empty_. But this is normally fine as it only
    /// happens if the `Vec<T>` is empty.
    ///
    pub fn try_from_smallvec(wrapped: SmallVec<A>) -> Result<Self, Size0Error> {
        if wrapped.is_empty() {
            Err(Size0Error)
        } else {
            Ok(Self(wrapped))
        }
    }

    /// See [`SmallVec::from_buf()`] but fails if the `buf` is empty.
    pub fn try_from_buf(buf: A) -> Result<Self, Size0Error> {
        Self::try_from_smallvec(SmallVec::from_buf(buf))
    }

    /// See [`SmallVec::from_buf_and_len()`] but fails if the buf and len are empty.
    ///
    /// # Panic
    ///
    /// Like [`SmallVec::from_buf_and_len()`] this fails if the length is > the
    /// size of the buffer. I.e. `SmallVec1::try_from_buf_and_len([] as [u8;0],2)` will
    /// panic.
    pub fn try_from_buf_and_len(buf: A, len: usize) -> Result<Self, Size0Error> {
        Self::try_from_smallvec(SmallVec::from_buf_and_len(buf, len))
    }

    /// Converts this instance into the underlying [`$wrapped<$t>`] instance.
    pub fn into_smallvec(self) -> SmallVec<A> {
        self.0
    }

    /// Return a reference to the underlying `$wrapped`.
    pub fn as_smallvec(&self) -> &SmallVec<A> {
        &self.0
    }

    /// Converts this instance into a [`Vec<$item_ty>`] instance.
    pub fn into_vec(self) -> Vec<A::Item> {
        self.0.into_vec()
    }

    /// Converts this instance into the inner most underlying buffer/array.
    ///
    /// This fails if the `SmallVec` has not the exact length of
    /// the underlying buffers/arrays capacity.
    ///
    /// This matches [`SmallVec::into_inner()`] in that if the
    //  length is to large or small self is returned as error.
    pub fn into_inner(self) -> Result<A, Self> {
        self.0.into_inner().map_err(SmallVec1)
    }

    /// See [`SmallVec::insert_many()`].
    pub fn insert_many<I: IntoIterator<Item = A::Item>>(&mut self, index: usize, iterable: I) {
        self.0.insert_many(index, iterable)
    }
}

impl<A> SmallVec1<A>
where
    A: Array,
    A::Item: Copy,
{
    pub fn try_from_slice(slice: &[A::Item]) -> Result<Self, Size0Error> {
        if slice.is_empty() {
            Err(Size0Error)
        } else {
            Ok(Self(SmallVec::from_slice(slice)))
        }
    }

    pub fn insert_from_slice(&mut self, index: usize, slice: &[A::Item]) {
        self.0.insert_from_slice(index, slice)
    }
}

impl<A> SmallVec1<A>
where
    A: Array,
    A::Item: Clone,
{
    pub fn try_from_elem(element: A::Item, len: usize) -> Result<Self, Size0Error> {
        if len == 0 {
            Err(Size0Error)
        } else {
            Ok(Self(SmallVec::from_elem(element, len)))
        }
    }
}

impl<T, const N: usize> SmallVec1<[T; N]> {
    /// Creates a new `SmallVec1` from an array.
    ///
    /// # Panics
    ///
    /// This will panic if N==0.
    pub const fn from_array_const(val: [T; N]) -> Self {
        if N == 0 {
            panic!("Empty arrays can not be used for creating a SmallVec1");
        }
        Self(SmallVec::from_const(val))
    }
}

impl_wrapper! {
    base_bounds_macro = A: Array,
    impl<A> SmallVec1<A> {
        fn inline_size(&self) -> usize;
        fn spilled(&self) -> bool;
        fn grow(&mut self, len: usize) -> ();
        fn try_reserve(&mut self, additional: usize) -> Result<(), CollectionAllocErr>;
        fn try_reserve_exact(&mut self, additional: usize) -> Result<(), CollectionAllocErr>;
        fn try_grow(&mut self, len: usize) -> Result<(), CollectionAllocErr>;
    }
}

impl<A, B> PartialEq<SmallVec1<B>> for SmallVec1<A>
where
    A::Item: PartialEq<B::Item>,
    A: Array,
    B: Array,
{
    #[inline]
    fn eq(&self, other: &SmallVec1<B>) -> bool {
        self.0.eq(&other.0)
    }
}

///FIXME(v2.0) use `From` and panic on `N==0` instead.
impl<T, const N: usize> TryFrom<[T; N]> for SmallVec1<[T; N]> {
    type Error = Size0Error;
    fn try_from(vec: [T; N]) -> Result<Self, Size0Error> {
        Self::try_from_buf(vec)
    }
}

impl<T, const N: usize> TryFrom<SmallVec1<[T; N]>> for [T; N] {
    type Error = SmallVec1<[T; N]>;
    fn try_from(vec: SmallVec1<[T; N]>) -> Result<Self, SmallVec1<[T; N]>> {
        vec.into_inner()
    }
}

impl<A> IntoIterator for SmallVec1<A>
where
    A: Array,
{
    type Item = A::Item;
    type IntoIter = smallvec::IntoIter<A>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<A> From<SmallVec1<A>> for Vec<A::Item>
where
    A: Array,
{
    fn from(vec: SmallVec1<A>) -> Vec<A::Item> {
        vec.into_vec()
    }
}

impl<A> TryFrom<Vec<A::Item>> for SmallVec1<A>
where
    A: Array,
{
    type Error = Size0Error;
    fn try_from(vec: Vec<A::Item>) -> Result<Self, Size0Error> {
        Self::try_from_vec(vec)
    }
}

impl<A> From<SmallVec1<A>> for Box<[A::Item]>
where
    A: Array,
{
    fn from(vec: SmallVec1<A>) -> Self {
        vec.into_boxed_slice()
    }
}

#[cfg(feature = "smallvec-v1-write")]
impl<A> io::Write for SmallVec1<A>
where
    A: Array<Item = u8>,
{
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.0.write_vectored(bufs)
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {

    mod SmallVec1 {
        #![allow(non_snake_case)]
        use super::super::*;
        use core::num::NonZeroUsize;
        use std::{
            borrow::{Borrow, BorrowMut, ToOwned},
            cmp::Ordering,
            collections::hash_map::DefaultHasher,
            format,
            hash::{Hash, Hasher},
            panic::catch_unwind,
            string::String,
            vec,
            vec::Vec,
        };

        #[test]
        fn Clone() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            let b = a.clone();
            assert_eq!(a, b);
        }

        #[test]
        fn Eq() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            let b: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            let c: SmallVec1<[u8; 4]> = smallvec1![2, 2, 3];

            assert_eq!(a, b);
            assert_ne!(a, c);
            //make sure Eq is supported and not only PartialEq
            fn cmp<A: Eq>() {}
            cmp::<SmallVec1<[u8; 4]>>();
        }

        #[test]
        fn PartialEq() {
            let a: SmallVec1<[String; 4]> = smallvec1!["hy".to_owned()];
            let b: SmallVec1<[&'static str; 4]> = smallvec1!["hy"];
            assert_eq!(a, b);

            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3, 4, 5];
            let b: SmallVec1<[u8; 8]> = smallvec1![1, 2, 3, 4, 5];
            assert_eq!(a, b);
        }

        #[test]
        fn Ord() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            let b: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(Ord::cmp(&a, &b), Ordering::Less);
        }

        #[test]
        fn Hash() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            let b = vec![1u8, 3];
            assert_eq!(compute_hash(&a), compute_hash(&b));

            /// -------------------
            fn compute_hash<T: Hash>(value: &T) -> u64 {
                let mut hasher = DefaultHasher::new();
                value.hash(&mut hasher);
                hasher.finish()
            }
        }

        #[test]
        fn Debug() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            assert_eq!(format!("{:?}", a), "[1, 2]");
        }

        #[test]
        fn Default() {
            let a = SmallVec1::<[u8; 4]>::default();
            assert_eq!(a.as_slice(), &[0u8] as &[u8]);
        }
        #[test]
        fn Deref() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            let _: &SmallVec<_> = a.as_smallvec();
            let b: &[u8] = &*a;
            assert_eq!(b, &[1u8, 2] as &[u8]);
        }

        #[test]
        fn DerefMut() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            let b: &mut [u8] = &mut *a;
            assert_eq!(b, &[1u8, 2] as &[u8]);
        }

        mod IntoIterator {
            use super::*;

            #[test]
            fn owned() {
                let a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
                let a_ = a.clone();
                let b = a.into_iter().collect::<Vec<_>>();
                assert_eq!(&a_[..], &b[..]);
            }

            #[test]
            fn by_ref() {
                let a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
                let a = (&a).into_iter().collect::<Vec<_>>();
                assert_eq!(a, vec![&12u8, &23]);
            }

            #[test]
            fn by_mut() {
                let mut a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
                let a = (&mut a).into_iter().collect::<Vec<_>>();
                assert_eq!(a, vec![&mut 12u8, &mut 23]);
            }
        }

        #[test]
        fn AsRef() {
            let a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            let _: &[u8] = a.as_ref();
            let _: &SmallVec<[u8; 4]> = a.as_ref();
        }

        mod AsMut {
            use super::{smallvec1, SmallVec1};

            #[test]
            fn os_slice() {
                let mut a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
                let _: &mut [u8] = a.as_mut();
            }

            #[test]
            fn of_self() {
                let mut a: SmallVec1<[u8; 4]> = smallvec1![33u8, 123];
                let v: &mut SmallVec1<[u8; 4]> = a.as_mut();
                let mut expected: SmallVec1<[u8; 4]> = smallvec1![33u8, 123];
                assert_eq!(v, &mut expected);
            }
        }

        #[test]
        fn Borrow() {
            let a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            let _: &[u8] = a.borrow();
            let _: &SmallVec<[u8; 4]> = a.borrow();
        }

        #[test]
        fn BorrowMut() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            let _: &mut [u8] = a.borrow_mut();
        }

        #[test]
        fn Extend() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            a.extend(vec![1u8, 2, 3].into_iter());
            assert_eq!(a.as_slice(), &[12u8, 23, 1, 2, 3] as &[u8]);
        }

        #[test]
        fn Index() {
            let a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            assert_eq!(a[0], 12);
        }

        #[test]
        fn IndexMut() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![12, 23];
            a[0] = 33;
            assert_eq!(a[0], 33);
        }

        mod TryFrom {
            use super::super::super::*;
            use std::{borrow::ToOwned, string::String, vec};

            #[test]
            fn slice() {
                let a =
                    SmallVec1::<[String; 4]>::try_from(&["hy".to_owned()] as &[String]).unwrap();
                assert_eq!(a[0], "hy");

                SmallVec1::<[String; 4]>::try_from(&[] as &[String]).unwrap_err();
            }

            #[test]
            fn misc() {
                let _ = SmallVec1::<[u8; 4]>::try_from(vec![1, 2, 3]).unwrap();
                let _ = SmallVec1::<[u8; 4]>::try_from(vec![]).unwrap_err();
                let _ = SmallVec1::<[u8; 4]>::try_from(smallvec![1, 2, 3]).unwrap();
                let _ = SmallVec1::<[u8; 4]>::try_from(smallvec![]).unwrap_err();
                let _ = SmallVec1::<[u8; 4]>::try_from([1u8, 2, 3, 4]).unwrap();
                let _ = SmallVec1::<[u8; 0]>::try_from([] as [u8; 0]).unwrap_err();
            }

            #[test]
            fn array_try_from_smallvec1() {
                let vec: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
                <[u8; 4]>::try_from(vec).unwrap();

                let vec: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2];
                <[u8; 4]>::try_from(vec).unwrap_err();
            }
        }

        #[test]
        fn new() {
            let a = SmallVec1::<[u8; 4]>::new(12);
            let b: SmallVec1<[u8; 4]> = smallvec1![12];
            assert_eq!(a, b);
        }

        #[test]
        fn with_capacity() {
            let a = SmallVec1::<[u8; 4]>::with_capacity(32, 21);
            assert_eq!(a.is_empty(), false);
            assert_eq!(a.capacity(), 21);

            let a = SmallVec1::<[u8; 4]>::with_capacity(32, 1);
            assert_eq!(a.is_empty(), false);
            assert_eq!(a.capacity(), 4 /*yes 4!*/);
        }

        #[test]
        fn try_from_vec() {
            let a = SmallVec1::<[u8; 4]>::try_from_vec(vec![1, 2, 3]);
            assert_eq!(a, Ok(smallvec1![1, 2, 3]));

            let b = SmallVec1::<[u8; 4]>::try_from_vec(vec![]);
            assert_eq!(b, Err(Size0Error));
        }

        #[test]
        fn try_from_smallvec() {
            let a = SmallVec1::<[u8; 4]>::try_from_smallvec(smallvec![32, 2, 3]);
            assert_eq!(a, Ok(smallvec1![32, 2, 3]));

            let a = SmallVec1::<[u8; 4]>::try_from_smallvec(smallvec![]);
            assert_eq!(a, Err(Size0Error));
        }

        #[test]
        fn try_from_buf() {
            let a = SmallVec1::try_from_buf([1u8, 2, 3, 4]);
            assert_eq!(a, Ok(smallvec1![1, 2, 3, 4]));

            let a = SmallVec1::try_from_buf([] as [u8; 0]);
            assert_eq!(a, Err(Size0Error));
        }

        #[test]
        fn try_from_buf_and_len() {
            let a = SmallVec1::try_from_buf_and_len([1u8, 2, 3, 4, 0, 0, 0, 0], 4);
            assert_eq!(a, Ok(smallvec1![1, 2, 3, 4]));

            let a = SmallVec1::try_from_buf_and_len([1u8, 2, 3], 0);
            assert_eq!(a, Err(Size0Error));
        }

        #[should_panic]
        #[test]
        fn try_from_buf_and_len_panic_if_len_gt_size() {
            let _ = SmallVec1::try_from_buf_and_len([] as [u8; 0], 3);
        }

        #[test]
        fn into_smallvec() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2];
            let a = a.into_smallvec();
            let b: SmallVec<[u8; 4]> = smallvec![1, 3, 2];
            assert_eq!(a, b);
        }

        #[test]
        fn into_vec() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2];
            let a: Vec<u8> = a.into_vec();
            assert_eq!(a, vec![1, 3, 2])
        }

        #[test]
        fn into_inner() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            let a: [u8; 4] = a.into_inner().unwrap();
            assert_eq!(a, [1, 3, 2, 4])
        }

        #[test]
        fn into_boxed_slice() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            let a: Box<[u8]> = a.into_boxed_slice();
            assert_eq!(&*a, &[1u8, 3, 2, 4] as &[u8])
        }

        #[test]
        fn leak() {
            let a: SmallVec1<[u8; 32]> = smallvec1![1u8, 3];
            let s: &'static mut [u8] = a.leak();
            assert_eq!(s, &[1u8, 3]);
        }

        #[test]
        fn reduce() {
            assert_eq!(smallvec1_inline![1u8, 2, 4, 3].reduce(std::cmp::max), 4);
            assert_eq!(smallvec1_inline![1u8, 2, 2, 3].reduce(|a, b| a + b), 8);
        }

        #[test]
        fn reduce_ref() {
            let a = smallvec1_inline![std::cell::Cell::new(4)];
            a.reduce_ref(std::cmp::max).set(44);
            assert_eq!(a, smallvec1_inline![std::cell::Cell::new(44)]);
        }

        #[test]
        fn reduce_mut() {
            let mut a = smallvec1_inline![1u8, 2, 4, 3];
            *a.reduce_mut(std::cmp::max) *= 2;
            assert_eq!(a, smallvec1_inline![1u8, 2, 8, 3]);
        }

        mod From {
            use super::*;

            #[test]
            fn boxed_slice_from_smallvec1() {
                let vec: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4, 5];
                let _ = Box::<[u8]>::from(vec);
            }

            #[test]
            fn vec_from_smallvec1() {
                let vec: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
                let _ = Vec::<u8>::from(vec);
            }

            #[test]
            fn smallvec_from_smallvec1() {
                let vec: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
                let _ = SmallVec::<[u8; 4]>::from(vec);
            }
        }

        #[test]
        fn last_first_methods_are_shadowed() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            assert_eq!(a.last(), &4);
            assert_eq!(a.last_mut(), &mut 4);
            assert_eq!(a.first(), &1);
            assert_eq!(a.first_mut(), &mut 1);
        }

        #[test]
        fn truncate() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            assert_eq!(a.truncate(0), Err(Size0Error));
            assert_eq!(a.truncate(1), Ok(()));
            assert_eq!(a.len(), 1);
        }

        #[test]
        fn try_truncate() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            assert_eq!(a.try_truncate(0), Err(Size0Error));
            assert_eq!(a.try_truncate(1), Ok(()));
            assert_eq!(a.len(), 1);
        }

        #[test]
        fn reserve() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            a.reserve(4);
            assert!(a.capacity() >= 8);
        }

        #[test]
        fn try_reserve() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            a.try_reserve(4).unwrap();
            assert!(a.capacity() >= 8);
        }

        #[test]
        fn reserve_exact() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            a.reserve_exact(4);
            assert_eq!(a.capacity(), 8);
        }

        #[test]
        fn try_reserve_exact() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4];
            a.try_reserve_exact(4).unwrap();
            assert_eq!(a.capacity(), 8);
        }

        #[test]
        fn shrink_to_fit() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 2, 4, 5];
            a.shrink_to_fit();
            assert_eq!(a.capacity(), 5);
        }

        #[test]
        fn push() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.push(12);
            let b: SmallVec1<[u8; 4]> = smallvec1![1, 3, 12];
            assert_eq!(a, b);
        }

        #[test]
        fn insert() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.insert(0, 12);
            let b: SmallVec1<[u8; 4]> = smallvec1![12, 1, 3];
            assert_eq!(a, b);
        }

        #[test]
        fn len() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.len(), 2);
        }

        #[test]
        fn len_nonzero() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.len_nonzero(), NonZeroUsize::new(2).unwrap());
        }

        #[test]
        fn capacity() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.capacity(), 4);
        }

        #[test]
        fn as_slice() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.as_slice(), &[1u8, 3] as &[u8]);
        }

        #[test]
        fn as_mut_slice() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.as_mut_slice()[0] = 10;
            let b: SmallVec1<[u8; 4]> = smallvec1![10, 3];
            assert_eq!(a, b);
        }

        #[test]
        fn inline_size() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.inline_size(), 4);
        }

        #[test]
        fn spilled() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.spilled(), false);

            let a: SmallVec1<[u8; 4]> = smallvec1![1, 3, 6, 9, 2];
            assert_eq!(a.spilled(), true);
        }

        #[test]
        fn pop() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.pop(), Ok(3));
            assert_eq!(a.pop(), Err(Size0Error));
        }

        #[test]
        fn try_pop() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.try_pop(), Ok(3));
            assert_eq!(a.try_pop(), Err(Size0Error));
        }

        #[test]
        fn append() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            let mut b: SmallVec<[u8; 4]> = smallvec![53, 12];
            a.append(&mut b);
            let c: SmallVec1<[u8; 4]> = smallvec1![1, 3, 53, 12];
            assert_eq!(a, c);
        }

        #[test]
        fn grow() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.grow(32);
            assert_eq!(a.capacity(), 32);
        }

        #[test]
        fn try_grow() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.try_grow(32).unwrap();
            assert_eq!(a.capacity(), 32);
        }

        #[test]
        fn swap_remove() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.swap_remove(0), Ok(1));
            assert_eq!(a.swap_remove(0), Err(Size0Error));
        }

        #[test]
        fn try_swap_remove() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.try_swap_remove(0), Ok(1));
            assert_eq!(a.try_swap_remove(0), Err(Size0Error));
        }

        #[test]
        fn remove() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.remove(0), Ok(1));
            assert_eq!(a.remove(0), Err(Size0Error));

            catch_unwind(|| {
                let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
                let _ = a.remove(200);
            })
            .unwrap_err();
        }

        #[test]
        fn try_remove() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            assert_eq!(a.try_remove(0), Ok(1));
            assert_eq!(a.try_remove(0), Err(Size0Error));
        }

        #[test]
        fn insert_many() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 3];
            a.insert_many(1, vec![2, 4, 8]);
            let b: SmallVec1<[u8; 4]> = smallvec1![1, 2, 4, 8, 3];
            assert_eq!(a, b);
        }

        #[test]
        fn dedup() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 1];
            a.dedup();
            assert_eq!(a.as_slice(), &[1u8] as &[u8]);
        }

        #[test]
        fn dedup_by() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 1, 4, 4];
            a.dedup_by(|a, b| a == b);
            assert_eq!(a.as_slice(), &[1u8, 4] as &[u8]);
        }

        #[test]
        fn dedup_by_key() {
            let mut a: SmallVec1<[(u8, u8); 4]> = smallvec1![(1, 2), (1, 5), (4, 4), (5, 4)];
            a.dedup_by_key(|a| a.0);
            assert_eq!(a.as_slice(), &[(1u8, 2u8), (4, 4), (5, 4)] as &[(u8, u8)]);
        }

        #[test]
        fn resize_with() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            assert_eq!(a.resize_with(0, Default::default), Err(Size0Error));
            assert_eq!(a.resize_with(4, Default::default), Ok(()));
        }

        #[test]
        fn try_resize_with() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            assert_eq!(a.try_resize_with(0, Default::default), Err(Size0Error));
            assert_eq!(a.try_resize_with(4, Default::default), Ok(()));
        }

        #[test]
        fn as_ptr() {
            let a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            let pa = a.as_ptr();
            let pb = a.as_slice().as_ptr();
            assert_eq!(pa as usize, pb as usize);
        }

        #[test]
        fn as_mut_ptr() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            let pa = a.as_mut_ptr();
            let pb = a.as_mut_slice().as_mut_ptr();
            assert_eq!(pa as usize, pb as usize);
        }

        #[test]
        fn try_from_slice() {
            let a = SmallVec1::<[u8; 4]>::try_from_slice(&[1u8, 2, 9]).unwrap();
            assert_eq!(a.as_slice(), &[1u8, 2, 9] as &[u8]);

            SmallVec1::<[u8; 4]>::try_from_slice(&[]).unwrap_err();
        }

        #[test]
        fn insert_from_slice() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            a.insert_from_slice(1, &[3, 9]);
            assert_eq!(a.as_slice(), &[1u8, 3, 9, 2] as &[u8]);
        }

        #[test]
        fn extend_from_slice() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2];
            a.extend_from_slice(&[3, 9]);
            assert_eq!(a.as_slice(), &[1u8, 2, 3, 9] as &[u8]);
        }

        #[test]
        fn resize() {
            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            assert_eq!(a.resize(0, 12), Err(Size0Error));
            assert_eq!(a.resize(2, 12), Ok(()));
            assert_eq!(a.resize(4, 12), Ok(()));
            assert_eq!(a.as_slice(), &[1u8, 2, 12, 12] as &[u8]);
        }

        #[test]
        fn try_resize() {
            #![allow(deprecated)]

            let mut a: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            assert_eq!(a.try_resize(0, 12), Err(Size0Error));
            assert_eq!(a.try_resize(2, 12), Ok(()));
            assert_eq!(a.try_resize(4, 12), Ok(()));
            assert_eq!(a.as_slice(), &[1u8, 2, 12, 12] as &[u8]);
        }

        #[test]
        fn try_from_elem() {
            let a = SmallVec1::<[u8; 4]>::try_from_elem(1u8, 3).unwrap();
            assert_eq!(a.as_slice(), &[1u8, 1, 1] as &[u8]);

            SmallVec1::<[u8; 4]>::try_from_elem(1u8, 0).unwrap_err();
        }

        #[test]
        fn split_off_first() {
            let a: SmallVec1<[u8; 4]> = smallvec1![32];
            assert_eq!((32, SmallVec::<[u8; 4]>::new()), a.split_off_first());

            let a: SmallVec1<[u8; 4]> = smallvec1![32, 43];
            let exp: SmallVec<[u8; 4]> = smallvec![43];
            assert_eq!((32, exp), a.split_off_first());
        }

        #[test]
        fn split_off_last() {
            let a: SmallVec1<[u8; 4]> = smallvec1![32];
            assert_eq!((SmallVec::<[u8; 4]>::new(), 32), a.split_off_last());

            let a: SmallVec1<[u8; 4]> = smallvec1![32, 43];
            let exp: SmallVec<[u8; 4]> = smallvec![32];
            assert_eq!((exp, 43), a.split_off_last());
        }

        #[test]
        fn from_vec_push() {
            let got: SmallVec1<[u8; 4]> = SmallVec1::from_vec_push(std::vec![], 1u8);
            let expected: SmallVec1<[u8; 4]> = smallvec1![1];
            assert_eq!(got, expected);
            let got: SmallVec1<[u8; 4]> = SmallVec1::from_vec_push(std::vec![1, 2], 3u8);
            let expected: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            assert_eq!(got, expected);
        }

        #[test]
        fn from_vec_insert() {
            let got: SmallVec1<[u8; 4]> = SmallVec1::from_vec_insert(std::vec![], 0, 1u8);
            let expected: SmallVec1<[u8; 4]> = smallvec1![1];
            assert_eq!(got, expected);
            let got: SmallVec1<[u8; 4]> = SmallVec1::from_vec_insert(std::vec![1, 3], 1, 2u8);
            let expected: SmallVec1<[u8; 4]> = smallvec1![1, 2, 3];
            assert_eq!(got, expected);
            assert!(catch_unwind(|| {
                SmallVec1::<[u8; 4]>::from_vec_insert(std::vec![1, 3], 3, 2u8);
            })
            .is_err());
        }

        #[cfg(feature = "serde")]
        mod serde {
            use super::super::super::*;

            #[test]
            fn can_be_serialized_and_deserialized() {
                let a: SmallVec1<[u8; 4]> = smallvec1![32, 12, 14, 18, 201];
                let json_str = serde_json::to_string(&a).unwrap();
                let b: SmallVec1<[u8; 4]> = serde_json::from_str(&json_str).unwrap();
                assert_eq!(a, b);
            }

            #[test]
            fn array_size_is_not_serialized() {
                let a: SmallVec1<[u8; 4]> = smallvec1![32, 12, 14, 18, 201];
                let json_str = serde_json::to_string(&a).unwrap();
                let b: SmallVec1<[u8; 8]> = serde_json::from_str(&json_str).unwrap();
                assert_eq!(a, b);
            }

            #[test]
            fn does_not_allow_empty_deserialization() {
                let a = Vec::<u8>::new();
                let json_str = serde_json::to_string(&a).unwrap();
                serde_json::from_str::<SmallVec1<[u8; 8]>>(&json_str).unwrap_err();
            }
        }
    }

    mod macros {
        use super::super::{smallvec1, smallvec1_inline, SmallVec1};

        #[test]
        fn smallvec1() {
            let _: SmallVec1<[u8; 2]> = smallvec1![1];
            let _: SmallVec1<[u8; 2]> = smallvec1![1,];
            let _: SmallVec1<[u8; 2]> = smallvec1![1, 2];
            let _: SmallVec1<[u8; 2]> = smallvec1![1, 2,];
        }

        #[test]
        fn smallvec1_inline() {
            assert_eq!(smallvec1_inline![1].capacity(), 1);
            assert_eq!(smallvec1_inline![1,].capacity(), 1);
            assert_eq!(smallvec1_inline![1, 2].capacity(), 2);
            assert_eq!(smallvec1_inline![1, 2,].capacity(), 2);
        }
    }
}
