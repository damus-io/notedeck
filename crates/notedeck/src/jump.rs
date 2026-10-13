//! Vim's jump list, the app's half: where an app's view is, as a token the
//! chrome keeps and hands back.
//!
//! The chrome owns the list and its keys (Ctrl-O back, Ctrl-I forward, and a
//! keymap binding marked `jump = true` recording where you were before it
//! fires). What a "location" is belongs to the app: it hands one out from
//! [`App::jump_location`](crate::App::jump_location) and is put back at it by
//! [`App::restore_jump`](crate::App::restore_jump). The chrome never looks
//! inside; it only asks two locations whether they are the same, so walking
//! back over a jump that went nowhere doesn't take a key press.

use std::any::Any;
use std::rc::Rc;

/// One location in an app's view. Any `'static` type with `PartialEq` is
/// one, so an app returns its own plain struct and downcasts it back with
/// [`as_any`](Self::as_any) when the chrome hands it in.
pub trait JumpTarget: Any {
    /// The location as `Any`, for the app to downcast to its own type.
    fn as_any(&self) -> &dyn Any;

    /// Whether `other` is this same location: same type, equal value.
    fn same(&self, other: &dyn JumpTarget) -> bool;
}

impl<T: Any + PartialEq> JumpTarget for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same(&self, other: &dyn JumpTarget) -> bool {
        other.as_any().downcast_ref::<T>() == Some(self)
    }
}

/// A location on the chrome's jump list: shared, so moving along the list
/// is a refcount bump rather than a copy.
pub type JumpLocation = Rc<dyn JumpTarget>;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(PartialEq)]
    struct Spot(u32);

    #[derive(PartialEq)]
    struct Other(u32);

    #[test]
    fn same_compares_type_and_value() {
        let a: JumpLocation = Rc::new(Spot(1));
        assert!(a.same(&Spot(1)));
        assert!(!a.same(&Spot(2)));
        assert!(!a.same(&Other(1)), "another type is never the same");
        assert_eq!(a.as_any().downcast_ref::<Spot>().map(|s| s.0), Some(1));
    }
}
