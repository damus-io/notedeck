//! Read a serde struct's field names without a value to read them from.
//!
//! `#[derive(Deserialize)]` on a struct with named fields calls
//! [`Deserializer::deserialize_struct`] with a `&'static` list of the names it
//! accepts, renames applied (Grep's `case_insensitive` arrives as `-i`) and
//! skipped fields left out. [`serde_fields`] hands the type a deserializer that
//! captures that list and stops, so a tool's [`ToolSpec`](super::ToolSpec) can
//! be checked against the struct its calls parse into (see
//! [`ToolSpec::arg_drift`](super::ToolSpec::arg_drift)).

use std::fmt;

use serde::de::{self, DeserializeOwned, Deserializer, Visitor};

/// The names `T` accepts as a JSON object's keys, exactly as serde reads them,
/// or `None` when `T` isn't a struct with named fields.
///
/// Renames are applied and `#[serde(skip)]`ped fields are left out. A field's
/// `#[serde(alias)]`es are listed beside its name, since serde lists them too.
/// An `Option<T>` or `Box<T>` reads through to `T`.
///
/// `None` for anything serde doesn't describe up front: a struct with a
/// `#[serde(flatten)]` field (serde reads it as a map), a map, an enum, a
/// tuple, a unit, a [`Value`](serde_json::Value).
pub fn serde_fields<T: DeserializeOwned>() -> Option<&'static [&'static str]> {
    match T::deserialize(FieldProbe) {
        Err(Probed::Fields(fields)) => Some(fields),
        Err(Probed::NotAStruct) | Ok(_) => None,
    }
}

/// A [`Deserializer`] that answers only "which fields?": it fails every read
/// with what it learned.
struct FieldProbe;

/// How a [`FieldProbe`] read ends: always as an "error", carrying the answer.
#[derive(Debug)]
enum Probed {
    /// A struct asked for these fields.
    Fields(&'static [&'static str]),
    /// It asked for something else, or failed on its own.
    NotAStruct,
}

impl fmt::Display for Probed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Probed::Fields(fields) => write!(f, "a struct of {fields:?}"),
            Probed::NotAStruct => f.write_str("not a struct with named fields"),
        }
    }
}

impl std::error::Error for Probed {}

impl de::Error for Probed {
    fn custom<M: fmt::Display>(_msg: M) -> Self {
        Probed::NotAStruct
    }
}

impl<'de> Deserializer<'de> for FieldProbe {
    type Error = Probed;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Probed> {
        Err(Probed::NotAStruct)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Probed> {
        Err(Probed::Fields(fields))
    }

    /// An optional struct takes the struct's fields.
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Probed> {
        visitor.visit_some(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct newtype_struct seq tuple
        tuple_struct map enum identifier ignored_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;
    use std::collections::HashMap;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(deny_unknown_fields)]
    struct Plain {
        path: String,
        limit: Option<u64>,
        #[serde(rename = "-i", default)]
        case_insensitive: bool,
        #[serde(skip)]
        cache: u8,
    }

    /// Names come out as the wire spells them: renamed, optional fields
    /// included, skipped ones gone.
    #[test]
    fn a_struct_lists_its_wire_names() {
        assert_eq!(serde_fields::<Plain>(), Some(&["path", "limit", "-i"][..]));
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    #[serde(rename_all = "camelCase")]
    struct Camel {
        file_path: String,
        #[serde(alias = "max")]
        head_limit: u64,
    }

    /// A container rename applies to every field, and an alias is listed
    /// after its field's name.
    #[test]
    fn rename_all_and_aliases() {
        assert_eq!(
            serde_fields::<Camel>(),
            Some(&["filePath", "headLimit", "max"][..])
        );
    }

    /// An optional or boxed struct takes the struct's fields.
    #[test]
    fn option_and_box_read_through() {
        let fields = serde_fields::<Plain>();
        assert_eq!(serde_fields::<Option<Plain>>(), fields);
        assert_eq!(serde_fields::<Box<Plain>>(), fields);
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Flattened {
        name: String,
        #[serde(flatten)]
        rest: HashMap<String, Value>,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    enum Tagged {
        A { x: u8 },
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Unit;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Empty {}

    /// What serde doesn't list up front reads as `None`, never as an empty
    /// list; a struct with no fields is an empty list.
    #[test]
    fn what_has_no_field_list_is_none() {
        assert_eq!(serde_fields::<Flattened>(), None);
        assert_eq!(serde_fields::<Tagged>(), None);
        assert_eq!(serde_fields::<Unit>(), None);
        assert_eq!(serde_fields::<()>(), None);
        assert_eq!(serde_fields::<Value>(), None);
        assert_eq!(serde_fields::<HashMap<String, u8>>(), None);
        assert_eq!(serde_fields::<(u8, u8)>(), None);
        assert_eq!(serde_fields::<i64>(), None);
        assert_eq!(serde_fields::<Empty>(), Some(&[][..]));
    }
}
