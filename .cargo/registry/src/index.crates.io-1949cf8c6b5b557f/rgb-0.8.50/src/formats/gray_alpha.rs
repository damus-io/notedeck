use crate::formats::gray_a::GrayA;
use core::ops::Deref;

#[repr(C)]
#[cfg_attr(feature = "unstable-experimental", deprecated(note = "renamed to GrayA"))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "defmt-03", derive(defmt::Format))]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
/// A pixel for grayscale value + alpha components (rgb crate v0.8)
///
/// Through a `Deref` hack it renames the fields from `.0` and `.1`
/// to `.v` (value) and `.a` (alpha)
#[allow(non_camel_case_types)]
pub struct GrayAlpha_v08<T, A = T>(
    /// Grayscale Component
    ///
    /// This field has been renamed to `.v`
    pub T,
    /// Alpha Component. This field has been renamed to `.a`.
    pub A,
);

impl<T: Copy> GrayAlpha_v08<T> {
    /// Reads the `.0` field
    ///
    /// This function isn't necessary, but it is forwards-compatible with the next major version of the RGB crate.
    pub fn value(self) -> T {
        self.0
    }

    /// Exposes the `.0` field for writing
    ///
    /// This function isn't necessary, but it is forwards-compatible with the next major version of the RGB crate.
    pub fn value_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T, A> Deref for GrayAlpha_v08<T, A> {
    type Target = GrayA<T, A>;

    /// A trick that allows using `.v` and `.a` on the old `GrayAlpha` type.
    fn deref(&self) -> &GrayA<T, A> {
        unsafe {
            &*(self as *const Self as *const GrayA::<T, A>)
        }
    }
}

#[test]
fn swizzle() {
    let g = GrayAlpha_v08(10u8, 20u8);
    assert_eq!(10, g.v);
    assert_eq!(20, g.a);
}
