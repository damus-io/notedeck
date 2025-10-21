// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Some macros that implement [`Readable`]/[`Writeable`] traits for lightning messages.
//! They also handle serialization and deserialization of TLVs.
//!
//! [`Readable`]: crate::util::ser::Readable
//! [`Writeable`]: crate::util::ser::Writeable

/// Implements serialization for a single TLV record.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _encode_tlv {
    ($stream: expr, $type: expr, $field: expr, (default_value, $default: expr) $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $field, required)
    };
    ($stream: expr, $type: expr, $field: expr, (static_value, $value: expr) $(, $self: ident)?) => {
        let _ = &$field; // Ensure we "use" the $field
    };
    ($stream: expr, $type: expr, $field: expr, required $(, $self: ident)?) => {
        BigSize($type).write($stream)?;
        BigSize($field.serialized_length() as u64).write($stream)?;
        $field.write($stream)?;
    };
    ($stream: expr, $type: expr, $field: expr, (required: $trait: ident $(, $read_arg: expr)?) $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $field, required);
    };
    ($stream: expr, $type: expr, $field: expr, required_vec $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $crate::util::ser::WithoutLength($field), required);
    };
    ($stream: expr, $type: expr, $field: expr, (required_vec, encoding: ($fieldty: ty, $encoding: ident)) $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $encoding($field), required);
    };
    ($stream: expr, $optional_type: expr, $optional_field: expr, option $(, $self: ident)?) => {
        if let Some(ref field) = $optional_field {
            BigSize($optional_type).write($stream)?;
            BigSize(field.serialized_length() as u64).write($stream)?;
            field.write($stream)?;
        }
    };
    ($stream: expr, $optional_type: expr, $optional_field: expr, (legacy, $fieldty: ty, $write: expr) $(, $self: ident)?) => { {
        let value: Option<_> = $write($($self)?);
        #[cfg(debug_assertions)]
        {
            // The value we write may be either an Option<$fieldty> or an Option<&$fieldty>.
            // Either way, it should decode just fine as a $fieldty, so we check that here.
            // This is useful in that it checks that we aren't accidentally writing, for example,
            // Option<Option<$fieldty>>.
            if let Some(v) = &value {
                let encoded_value = v.encode();
                let mut read_slice = &encoded_value[..];
                let _: $fieldty = $crate::util::ser::Readable::read(&mut read_slice)
                    .expect("Failed to read written TLV, check types");
                assert!(read_slice.is_empty(), "Reading written TLV was short, check types");
            }
        }
        $crate::_encode_tlv!($stream, $optional_type, value, option);
    } };
    ($stream: expr, $type: expr, $field: expr, optional_vec $(, $self: ident)?) => {
        if !$field.is_empty() {
            $crate::_encode_tlv!($stream, $type, $field, required_vec);
        }
    };
    ($stream: expr, $type: expr, $field: expr, upgradable_required $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $field, required);
    };
    ($stream: expr, $type: expr, $field: expr, upgradable_option $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $field, option);
    };
    ($stream: expr, $type: expr, $field: expr, (option, encoding: ($fieldty: ty, $encoding: ident) $(, $self: ident)?)) => {
        $crate::_encode_tlv!($stream, $type, $field.map(|f| $encoding(f)), option);
    };
    ($stream: expr, $type: expr, $field: expr, (option, encoding: $fieldty: ty) $(, $self: ident)?) => {
        $crate::_encode_tlv!($stream, $type, $field, option);
    };
    ($stream: expr, $type: expr, $field: expr, (option: $trait: ident $(, $read_arg: expr)?) $(, $self: ident)?) => {
        // Just a read-mapped type
        $crate::_encode_tlv!($stream, $type, $field, option);
    };
}

/// Panics if the last seen TLV type is not numerically less than the TLV type currently being checked.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _check_encoded_tlv_order {
    ($last_type: expr, $type: expr, (static_value, $value: expr)) => {};
    ($last_type: expr, $type: expr, $fieldty: tt) => {
        if let Some(t) = $last_type {
            // Note that $type may be 0 making the following comparison always false
            #[allow(unused_comparisons)]
            (debug_assert!(t < $type))
        }
        $last_type = Some($type);
    };
}

/// Implements the TLVs serialization part in a [`Writeable`] implementation of a struct.
///
/// This should be called inside a method which returns `Result<_, `[`io::Error`]`>`, such as
/// [`Writeable::write`]. It will only return an `Err` if the stream `Err`s or [`Writeable::write`]
/// on one of the fields `Err`s.
///
/// `$stream` must be a `&mut `[`Writer`] which will receive the bytes for each TLV in the stream.
///
/// Fields MUST be sorted in `$type`-order.
///
/// Note that the lightning TLV requirements require that a single type not appear more than once,
/// that TLVs are sorted in type-ascending order, and that any even types be understood by the
/// decoder.
///
/// Any `option` fields which have a value of `None` will not be serialized at all.
///
/// For example,
/// ```
/// let mut required_value = 0u64;
/// let mut optional_value: Option<u64> = None;
/// // At this point `required_value` has been written as a TLV of type 0, `42u64` has been written
/// // as a TLV of type 1 (indicating the reader may ignore it if it is not understood), and *no*
/// // TLV is written with type 2.
/// ```
///
/// [`Writeable`]: crate::util::ser::Writeable
/// [`io::Error`]: crate::io::Error
/// [`Writeable::write`]: crate::util::ser::Writeable::write
/// [`Writer`]: crate::util::ser::Writer
#[macro_export]
macro_rules! encode_tlv_stream {
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt)),* $(,)*}) => {
        $crate::_encode_tlv_stream!($stream, {$(($type, $field, $fieldty)),*})
    }
}

/// Implementation of [`encode_tlv_stream`].
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _encode_tlv_stream {
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt $(, $self: ident)?)),* $(,)*}) => { {
        $crate::_encode_tlv_stream!($stream, { $(($type, $field, $fieldty $(, $self)?)),* }, &[])
    } };
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt $(, $self: ident)?)),* $(,)*}, $extra_tlvs: expr) => { {
        #[allow(unused_imports)]
        use $crate::{
            ln::msgs::DecodeError,
            util::ser,
            util::ser::BigSize,
            util::ser::Writeable,
        };

        $(
            $crate::_encode_tlv!($stream, $type, $field, $fieldty $(, $self)?);
        )*
        for tlv in $extra_tlvs {
            let (typ, value): &(u64, Vec<u8>) = tlv;
            $crate::_encode_tlv!($stream, *typ, value, required_vec);
        }

        #[allow(unused_mut, unused_variables, unused_assignments)]
        #[cfg(debug_assertions)]
        {
            let mut last_seen: Option<u64> = None;
            $(
                $crate::_check_encoded_tlv_order!(last_seen, $type, $fieldty);
            )*
            for tlv in $extra_tlvs {
                let (typ, _): &(u64, Vec<u8>) = tlv;
                $crate::_check_encoded_tlv_order!(last_seen, *typ, required_vec);
            }
        }
    } };
}

/// Adds the length of the serialized field to a [`LengthCalculatingWriter`].
/// This is exported for use by other exported macros, do not use directly.
///
/// [`LengthCalculatingWriter`]: crate::util::ser::LengthCalculatingWriter
#[doc(hidden)]
#[macro_export]
macro_rules! _get_varint_length_prefixed_tlv_length {
    ($len: expr, $type: expr, $field: expr, (default_value, $default: expr) $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, required)
    };
    ($len: expr, $type: expr, $field: expr, (static_value, $value: expr) $(, $self: ident)?) => {};
    ($len: expr, $type: expr, $field: expr, required $(, $self: ident)?) => {
        BigSize($type).write(&mut $len).expect("No in-memory data may fail to serialize");
        let field_len = $field.serialized_length();
        BigSize(field_len as u64)
            .write(&mut $len)
            .expect("No in-memory data may fail to serialize");
        $len.0 += field_len;
    };
    ($len: expr, $type: expr, $field: expr, (required: $trait: ident $(, $read_arg: expr)?) $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, required);
    };
    ($len: expr, $type: expr, $field: expr, required_vec $(, $self: ident)?) => {
        let field = $crate::util::ser::WithoutLength($field);
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, field, required);
    };
    ($len: expr, $type: expr, $field: expr, (required_vec, encoding: ($fieldty: ty, $encoding: ident)) $(, $self: ident)?) => {
        let field = $encoding($field);
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, field, required);
    };
    ($len: expr, $optional_type: expr, $optional_field: expr, option $(, $self: ident)?) => {
        if let Some(ref field) = $optional_field.as_ref() {
            BigSize($optional_type)
                .write(&mut $len)
                .expect("No in-memory data may fail to serialize");
            let field_len = field.serialized_length();
            BigSize(field_len as u64)
                .write(&mut $len)
                .expect("No in-memory data may fail to serialize");
            $len.0 += field_len;
        }
    };
    ($len: expr, $optional_type: expr, $optional_field: expr, (legacy, $fieldty: ty, $write: expr) $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $optional_type, $write($($self)?), option);
    };
    ($len: expr, $type: expr, $field: expr, optional_vec $(, $self: ident)?) => {
        if !$field.is_empty() {
            $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, required_vec);
        }
    };
    ($len: expr, $type: expr, $field: expr, (option: $trait: ident $(, $read_arg: expr)?) $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, option);
    };
    ($len: expr, $type: expr, $field: expr, (option, encoding: ($fieldty: ty, $encoding: ident)) $(, $self: ident)?) => {
        let field = $field.map(|f| $encoding(f));
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, field, option);
    };
    ($len: expr, $type: expr, $field: expr, upgradable_required $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, required);
    };
    ($len: expr, $type: expr, $field: expr, upgradable_option $(, $self: ident)?) => {
        $crate::_get_varint_length_prefixed_tlv_length!($len, $type, $field, option);
    };
}

/// See the documentation of [`write_tlv_fields`].
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _encode_varint_length_prefixed_tlv {
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt $(, $self: ident)?)),*}) => { {
        $crate::_encode_varint_length_prefixed_tlv!($stream, {$(($type, $field, $fieldty $(, $self)?)),*}, &[])
    } };
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt $(, $self: ident)?)),*}, $extra_tlvs: expr) => { {
        extern crate alloc;
        use $crate::util::ser::BigSize;
        use alloc::vec::Vec;
        let len = {
            #[allow(unused_mut)]
            let mut len = $crate::util::ser::LengthCalculatingWriter(0);
            $(
                $crate::_get_varint_length_prefixed_tlv_length!(len, $type, $field, $fieldty $(, $self)?);
            )*
            for tlv in $extra_tlvs {
                let (typ, value): &(u64, Vec<u8>) = tlv;
                $crate::_get_varint_length_prefixed_tlv_length!(len, *typ, value, required_vec);
            }
            len.0
        };
        BigSize(len as u64).write($stream)?;
        $crate::_encode_tlv_stream!($stream, { $(($type, $field, $fieldty $(, $self)?)),* }, $extra_tlvs);
    } };
}

/// Errors if there are missing required TLV types between the last seen type and the type currently being processed.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _check_decoded_tlv_order {
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (default_value, $default: expr)) => {{
        // Note that $type may be 0 making the second comparison always false
        #[allow(unused_comparisons)]
        let invalid_order =
            ($last_seen_type.is_none() || $last_seen_type.unwrap() < $type) && $typ.0 > $type;
        if invalid_order {
            $field = $default.into();
        }
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (static_value, $value: expr)) => {};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, required) => {{
        // Note that $type may be 0 making the second comparison always false
        #[allow(unused_comparisons)]
        let invalid_order =
            ($last_seen_type.is_none() || $last_seen_type.unwrap() < $type) && $typ.0 > $type;
        if invalid_order {
            return Err(DecodeError::InvalidValue);
        }
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (required: $trait: ident $(, $read_arg: expr)?)) => {{
        $crate::_check_decoded_tlv_order!($last_seen_type, $typ, $type, $field, required);
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, option) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (option, explicit_type: $fieldty: ty)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (legacy, $fieldty: ty, $write: expr)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (required, explicit_type: $fieldty: ty)) => {{
        _check_decoded_tlv_order!($last_seen_type, $typ, $type, $field, required);
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, required_vec) => {{
        $crate::_check_decoded_tlv_order!($last_seen_type, $typ, $type, $field, required);
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (required_vec, encoding: $encoding: tt)) => {{
        $crate::_check_decoded_tlv_order!($last_seen_type, $typ, $type, $field, required);
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, optional_vec) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, upgradable_required) => {{ _check_decoded_tlv_order!($last_seen_type, $typ, $type, $field, required) }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, upgradable_option) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (option: $trait: ident $(, $read_arg: expr)?)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $typ: expr, $type: expr, $field: ident, (option, encoding: $encoding: tt)) => {{
        // no-op
    }};
}

/// Errors if there are missing required TLV types after the last seen type.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _check_missing_tlv {
    ($last_seen_type: expr, $type: expr, $field: ident, (default_value, $default: expr)) => {{
        // Note that $type may be 0 making the second comparison always false
        #[allow(unused_comparisons)]
        let missing_req_type = $last_seen_type.is_none() || $last_seen_type.unwrap() < $type;
        if missing_req_type {
            $field = $default.into();
        }
    }};
    ($last_seen_type: expr, $type: expr, $field: expr, (static_value, $value: expr)) => {
        $field = $value;
    };
    ($last_seen_type: expr, $type: expr, $field: ident, required) => {{
        // Note that $type may be 0 making the second comparison always false
        #[allow(unused_comparisons)]
        let missing_req_type = $last_seen_type.is_none() || $last_seen_type.unwrap() < $type;
        if missing_req_type {
            return Err(DecodeError::InvalidValue);
        }
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (required: $trait: ident $(, $read_arg: expr)?)) => {{
        $crate::_check_missing_tlv!($last_seen_type, $type, $field, required);
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, required_vec) => {{
        $crate::_check_missing_tlv!($last_seen_type, $type, $field, required);
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (required_vec, encoding: $encoding: tt)) => {{
        $crate::_check_missing_tlv!($last_seen_type, $type, $field, required);
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, option) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (option, explicit_type: $fieldty: ty)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (legacy, $fieldty: ty, $write: expr)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (required, explicit_type: $fieldty: ty)) => {{
        _check_missing_tlv!($last_seen_type, $type, $field, required);
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, optional_vec) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, upgradable_required) => {{ _check_missing_tlv!($last_seen_type, $type, $field, required) }};
    ($last_seen_type: expr, $type: expr, $field: ident, upgradable_option) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (option: $trait: ident $(, $read_arg: expr)?)) => {{
        // no-op
    }};
    ($last_seen_type: expr, $type: expr, $field: ident, (option, encoding: $encoding: tt)) => {{
        // no-op
    }};
}

/// Implements deserialization for a single TLV record.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _decode_tlv {
    ($outer_reader: expr, $reader: expr, $field: ident, (default_value, $default: expr)) => {{
        $crate::_decode_tlv!($outer_reader, $reader, $field, required)
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (static_value, $value: expr)) => {{
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, required) => {{
        $field = $crate::util::ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?;
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (required: $trait: ident $(, $read_arg: expr)?)) => {{
        $field = $trait::read(&mut $reader $(, $read_arg)*)?;
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, required_vec) => {{
        let f: $crate::util::ser::WithoutLength<Vec<_>> = $crate::util::ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?;
        $field = f.0;
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (required_vec, encoding: ($fieldty: ty, $encoding: ident))) => {{
        $field = {
            let field: $encoding<$fieldty> = ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?;
            $crate::util::ser::RequiredWrapper(Some(field.0))
        };
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, option) => {{
        $field = Some($crate::util::ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?);
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (option, explicit_type: $fieldty: ty)) => {{
        let _field: &Option<$fieldty> = &$field;
        $crate::_decode_tlv!($outer_reader, $reader, $field, option);
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (legacy, $fieldty: ty, $write: expr)) => {{
        $crate::_decode_tlv!($outer_reader, $reader, $field, (option, explicit_type: $fieldty));
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (required, explicit_type: $fieldty: ty)) => {{
        let _field: &$fieldty = &$field;
        _decode_tlv!($outer_reader, $reader, $field, required);
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, optional_vec) => {{
        let f: $crate::util::ser::WithoutLength<Vec<_>> = $crate::util::ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?;
        $field = Some(f.0);
    }};
    // `upgradable_required` indicates we're reading a required TLV that may have been upgraded
    // without backwards compat. We'll error if the field is missing, and return `Ok(None)` if the
    // field is present but we can no longer understand it.
    // Note that this variant can only be used within a `MaybeReadable` read.
    ($outer_reader: expr, $reader: expr, $field: ident, upgradable_required) => {{
        $field = match $crate::util::ser::MaybeReadable::read(&mut $reader)? {
            Some(res) => res,
            None => {
                // If we successfully read a value but we don't know how to parse it, we give up
                // and immediately return `None`. However, we need to make sure we read the correct
                // number of bytes for this TLV stream, which is implicitly the end of the stream.
                // Thus, we consume everything left in the `$outer_reader` here, ensuring that if
                // we're being read as a part of another TLV stream we don't spuriously fail to
                // deserialize the outer object due to a TLV length mismatch.
                $crate::io_extras::copy($outer_reader, &mut $crate::io_extras::sink()).unwrap();
                return Ok(None)
            },
        };
    }};
    // `upgradable_option` indicates we're reading an Option-al TLV that may have been upgraded
    // without backwards compat. $field will be None if the TLV is missing or if the field is present
    // but we can no longer understand it.
    ($outer_reader: expr, $reader: expr, $field: ident, upgradable_option) => {{
        $field = $crate::util::ser::MaybeReadable::read(&mut $reader)?;
        if $field.is_none() {
            #[cfg(not(debug_assertions))] {
                // In general, MaybeReadable implementations are required to consume all the bytes
                // of the object even if they don't understand it, but due to a bug in the
                // serialization format for `impl_writeable_tlv_based_enum_upgradable` we sometimes
                // don't know how many bytes that is. In such cases, we'd like to spuriously allow
                // TLV length mismatches, which we do here by calling `eat_remaining` so that the
                // `s.bytes_remain()` check in `_decode_tlv_stream_range` doesn't fail.
                $reader.eat_remaining()?;
            }
        }
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (option: $trait: ident $(, $read_arg: expr)?)) => {{
        $field = Some($trait::read(&mut $reader $(, $read_arg)*)?);
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (option, encoding: ($fieldty: ty, $encoding: ident, $encoder:ty))) => {{
        $crate::_decode_tlv!($outer_reader, $reader, $field, (option, encoding: ($fieldty, $encoding)));
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (option, encoding: ($fieldty: ty, $encoding: ident))) => {{
        $field = {
            let field: $encoding<$fieldty> = ser::LengthReadable::read_from_fixed_length_buffer(&mut $reader)?;
            Some(field.0)
        };
    }};
    ($outer_reader: expr, $reader: expr, $field: ident, (option, encoding: $fieldty: ty)) => {{
        $crate::_decode_tlv!($outer_reader, $reader, $field, option);
    }};
}

/// Checks if `$val` matches `$type`.
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _decode_tlv_stream_match_check {
    ($val: ident, $type: expr, (static_value, $value: expr)) => {
        false
    };
    ($val: ident, $type: expr, $fieldty: tt) => {
        $val == $type
    };
}

/// Implements the TLVs deserialization part in a [`Readable`] implementation of a struct.
///
/// This should be called inside a method which returns `Result<_, `[`DecodeError`]`>`, such as
/// [`Readable::read`]. It will either return an `Err` or ensure all `required` fields have been
/// read and optionally read `optional` fields.
///
/// `$stream` must be a [`Read`] and will be fully consumed, reading until no more bytes remain
/// (i.e. it returns [`DecodeError::ShortRead`]).
///
/// Fields MUST be sorted in `$type`-order.
///
/// Note that the lightning TLV requirements require that a single type not appear more than once,
/// that TLVs are sorted in type-ascending order, and that any even types be understood by the
/// decoder.
///
/// For example,
/// ```
/// let mut required_value = 0u64;
/// let mut optional_value: Option<u64> = None;
/// // At this point, `required_value` has been overwritten with the TLV with type 0.
/// // `optional_value` may have been overwritten, setting it to `Some` if a TLV with type 2 was
/// // present.
/// ```
///
/// [`Readable`]: crate::util::ser::Readable
/// [`DecodeError`]: crate::ln::msgs::DecodeError
/// [`Readable::read`]: crate::util::ser::Readable::read
/// [`Read`]: crate::io::Read
/// [`DecodeError::ShortRead`]: crate::ln::msgs::DecodeError::ShortRead
#[macro_export]
macro_rules! decode_tlv_stream {
    ($stream: expr, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => {
        let rewind = |_, _| { unreachable!() };
        $crate::_decode_tlv_stream_range!($stream, .., rewind, {$(($type, $field, $fieldty)),*});
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! _decode_tlv_stream_range {
    ($stream: expr, $range: expr, $rewind: ident, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}
     $(, $decode_custom_tlv: expr)?) => { {
        use $crate::ln::msgs::DecodeError;
        let mut last_seen_type: Option<u64> = None;
        let stream_ref = $stream;
        'tlv_read: loop {
            use $crate::util::ser;

            // First decode the type of this TLV:
            let typ: ser::BigSize = {
                // We track whether any bytes were read during the consensus_decode call to
                // determine whether we should break or return ShortRead if we get an
                // UnexpectedEof. This should in every case be largely cosmetic, but its nice to
                // pass the TLV test vectors exactly, which require this distinction.
                let mut tracking_reader = ser::ReadTrackingReader::new(stream_ref);
                match <$crate::util::ser::BigSize as $crate::util::ser::Readable>::read(&mut tracking_reader) {
                    Err(DecodeError::ShortRead) => {
                        if !tracking_reader.have_read {
                            break 'tlv_read;
                        } else {
                            return Err(DecodeError::ShortRead);
                        }
                    },
                    Err(e) => return Err(e),
                    Ok(t) => if core::ops::RangeBounds::contains(&$range, &t.0) { t } else {
                        drop(tracking_reader);

                        // Assumes the type id is minimally encoded, which is enforced on read.
                        use $crate::util::ser::Writeable;
                        let bytes_read = t.serialized_length();
                        $rewind(stream_ref, bytes_read);
                        break 'tlv_read;
                    },
                }
            };

            // Types must be unique and monotonically increasing:
            match last_seen_type {
                Some(t) if typ.0 <= t => {
                    return Err(DecodeError::InvalidValue);
                },
                _ => {},
            }
            // As we read types, make sure we hit every required type between `last_seen_type` and `typ`:
            $({
                $crate::_check_decoded_tlv_order!(last_seen_type, typ, $type, $field, $fieldty);
            })*
            last_seen_type = Some(typ.0);

            // Finally, read the length and value itself:
            let length: ser::BigSize = $crate::util::ser::Readable::read(stream_ref)?;
            let mut s = ser::FixedLengthReader::new(stream_ref, length.0);
            match typ.0 {
                $(_t if $crate::_decode_tlv_stream_match_check!(_t, $type, $fieldty) => {
                    $crate::_decode_tlv!($stream, s, $field, $fieldty);
                    if s.bytes_remain() {
                        s.eat_remaining()?; // Return ShortRead if there's actually not enough bytes
                        return Err(DecodeError::InvalidValue);
                    }
                },)*
                t => {
                    $(
                        if $decode_custom_tlv(t, &mut s)? {
                            // If a custom TLV was successfully read (i.e. decode_custom_tlv returns true),
                            // continue to the next TLV read.
                            s.eat_remaining()?;
                            continue 'tlv_read;
                        }
                    )?
                    if t % 2 == 0 {
                        return Err(DecodeError::UnknownRequiredFeature);
                    }
                }
            }
            s.eat_remaining()?;
        }
        // Make sure we got to each required type after we've read every TLV:
        $({
            $crate::_check_missing_tlv!(last_seen_type, $type, $field, $fieldty);
        })*
    } }
}

/// Implements [`LengthReadable`]/[`Writeable`] for a message struct that may include non-TLV and
/// TLV-encoded parts.
///
/// This is useful to implement a [`CustomMessageReader`].
///
/// Currently `$fieldty` may only be `option`, i.e., `$tlvfield` is optional field.
///
/// For example,
/// ```
/// struct MyCustomMessage {
///     pub field_1: u32,
///     pub field_2: bool,
///     pub field_3: String,
///     pub tlv_optional_integer: Option<u32>,
/// }
///
/// ```
///
/// [`LengthReadable`]: crate::util::ser::LengthReadable
/// [`Writeable`]: crate::util::ser::Writeable
/// [`CustomMessageReader`]: crate::ln::wire::CustomMessageReader
#[macro_export]
macro_rules! impl_writeable_msg {
    ($st:ident, {$($field:ident),* $(,)*}, {$(($type: expr, $tlvfield: ident, $fieldty: tt)),* $(,)*}) => {
        impl $crate::util::ser::Writeable for $st {
            fn write<W: $crate::util::ser::Writer>(&self, w: &mut W) -> Result<(), $crate::io::Error> {
                $( self.$field.write(w)?; )*
                $crate::encode_tlv_stream!(w, {$(($type, self.$tlvfield.as_ref(), $fieldty)),*});
                Ok(())
            }
        }
        impl $crate::util::ser::LengthReadable for $st {
            fn read_from_fixed_length_buffer<R: $crate::util::ser::LengthLimitedRead>(
                r: &mut R
            ) -> Result<Self, $crate::ln::msgs::DecodeError> {
                $(let $field = $crate::util::ser::Readable::read(r)?;)*
                $($crate::_init_tlv_field_var!($tlvfield, $fieldty);)*
                $crate::decode_tlv_stream!(r, {$(($type, $tlvfield, $fieldty)),*});
                Ok(Self {
                    $($field,)*
                    $($tlvfield),*
                })
            }
        }
    }
}

/// Writes out a suffix to an object as a length-prefixed TLV stream which contains potentially
/// backwards-compatible, optional fields which old nodes can happily ignore.
///
/// It is written out in TLV format and, as with all TLV fields, unknown even fields cause a
/// [`DecodeError::UnknownRequiredFeature`] error, with unknown odd fields ignored.
///
/// This is the preferred method of adding new fields that old nodes can ignore and still function
/// correctly.
///
/// [`DecodeError::UnknownRequiredFeature`]: crate::ln::msgs::DecodeError::UnknownRequiredFeature
#[macro_export]
macro_rules! write_tlv_fields {
    ($stream: expr, {$(($type: expr, $field: expr, $fieldty: tt)),* $(,)*}) => {
        $crate::_encode_varint_length_prefixed_tlv!($stream, {$(($type, &$field, $fieldty)),*})
    }
}

/// Reads a suffix added by [`write_tlv_fields`].
///
/// [`write_tlv_fields`]: crate::write_tlv_fields
#[macro_export]
macro_rules! read_tlv_fields {
    ($stream: expr, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => { {
        let tlv_len: $crate::util::ser::BigSize = $crate::util::ser::Readable::read($stream)?;
        let mut rd = $crate::util::ser::FixedLengthReader::new($stream, tlv_len.0);
        $crate::decode_tlv_stream!(&mut rd, {$(($type, $field, $fieldty)),*});
        rd.eat_remaining().map_err(|_| $crate::ln::msgs::DecodeError::ShortRead)?;
    } }
}

/// Initializes the struct fields.
///
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _init_tlv_based_struct_field {
    ($field: ident, (default_value, $default: expr)) => {
        $field.0.unwrap()
    };
    ($field: ident, (static_value, $value: expr)) => {
        $field
    };
    ($field: ident, option) => {
        $field
    };
    ($field: ident, (legacy, $fieldty: ty, $write: expr)) => {
        $crate::_init_tlv_based_struct_field!($field, option)
    };
    ($field: ident, (option: $trait: ident $(, $read_arg: expr)?)) => {
        $crate::_init_tlv_based_struct_field!($field, option)
    };
    // Note that legacy TLVs are eaten by `drop_legacy_field_definition`
    ($field: ident, upgradable_required) => {
        $field.0.unwrap()
    };
    ($field: ident, upgradable_option) => {
        $field
    };
    ($field: ident, required) => {
        $field.0.unwrap()
    };
    ($field: ident, (required: $trait: ident $(, $read_arg: expr)?)) => {
        $crate::_init_tlv_based_struct_field!($field, required)
    };
    ($field: ident, required_vec) => {
        $field
    };
    ($field: ident, (required_vec, encoding: ($fieldty: ty, $encoding: ident))) => {
        $crate::_init_tlv_based_struct_field!($field, required)
    };
    ($field: ident, optional_vec) => {
        $field.unwrap()
    };
}

/// Initializes the variable we are going to read the TLV into.
///
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _init_tlv_field_var {
    ($field: ident, (default_value, $default: expr)) => {
        let mut $field = $crate::util::ser::RequiredWrapper(None);
    };
    ($field: ident, (static_value, $value: expr)) => {
        let $field;
    };
    ($field: ident, required) => {
        let mut $field = $crate::util::ser::RequiredWrapper(None);
    };
    ($field: ident, (required: $trait: ident $(, $read_arg: expr)?)) => {
        $crate::_init_tlv_field_var!($field, required);
    };
    ($field: ident, required_vec) => {
        let mut $field = Vec::new();
    };
    ($field: ident, (required_vec, encoding: ($fieldty: ty, $encoding: ident))) => {
        $crate::_init_tlv_field_var!($field, required);
    };
    ($field: ident, option) => {
        let mut $field = None;
    };
    ($field: ident, optional_vec) => {
        let mut $field = Some(Vec::new());
    };
    ($field: ident, (option, explicit_type: $fieldty: ty)) => {
        let mut $field: Option<$fieldty> = None;
    };
    ($field: ident, (legacy, $fieldty: ty, $write: expr)) => {
        $crate::_init_tlv_field_var!($field, (option, explicit_type: $fieldty));
    };
    ($field: ident, (required, explicit_type: $fieldty: ty)) => {
        let mut $field = $crate::util::ser::RequiredWrapper::<$fieldty>(None);
    };
    ($field: ident, (option, encoding: ($fieldty: ty, $encoding: ident))) => {
        $crate::_init_tlv_field_var!($field, option);
    };
    ($field: ident, (option: $trait: ident $(, $read_arg: expr)?)) => {
        $crate::_init_tlv_field_var!($field, option);
    };
    ($field: ident, upgradable_required) => {
        let mut $field = $crate::util::ser::UpgradableRequired(None);
    };
    ($field: ident, upgradable_option) => {
        let mut $field = None;
    };
}

/// Equivalent to running [`_init_tlv_field_var`] then [`read_tlv_fields`].
///
/// If any unused values are read, their type MUST be specified or else `rustc` will read them as an
/// `i64`.
///
/// This is exported for use by other exported macros, do not use directly.
#[doc(hidden)]
#[macro_export]
macro_rules! _init_and_read_len_prefixed_tlv_fields {
    ($reader: ident, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => {
        $(
            $crate::_init_tlv_field_var!($field, $fieldty);
        )*

        $crate::read_tlv_fields!($reader, {
            $(($type, $field, $fieldty)),*
        });
    }
}

/// Equivalent to running [`_init_tlv_field_var`] then [`decode_tlv_stream`].
///
/// If any unused values are read, their type MUST be specified or else `rustc` will read them as an
/// `i64`.
macro_rules! _init_and_read_tlv_stream {
    ($reader: ident, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => {
        $(
            $crate::_init_tlv_field_var!($field, $fieldty);
        )*
        $crate::decode_tlv_stream!($reader, {
            $(($type, $field, $fieldty)),*
        });
    }
}

/// Reads a TLV stream with the given fields to build a struct/enum variant of type `$thing`
#[doc(hidden)]
#[macro_export]
macro_rules! _decode_and_build {
    ($stream: ident, $thing: path, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => { {
        $crate::_init_and_read_len_prefixed_tlv_fields!($stream, {
            $(($type, $field, $fieldty)),*
        });
        ::lightning_macros::drop_legacy_field_definition!($thing {
            $($field: $crate::_init_tlv_based_struct_field!($field, $fieldty)),*
        })
    } }
}

/// Implements [`Readable`]/[`Writeable`] for a struct storing it as a set of TLVs. Each TLV is
/// read/written in the order they appear and contains a type number, a field name, and a
/// de/serialization method, from the following:
///
/// If `$fieldty` is `required`, then `$field` is a required field that is not an [`Option`] nor a [`Vec`].
/// If `$fieldty` is `(default_value, $default)`, then `$field` will be set to `$default` if not present.
/// If `$fieldty` is `(static_value, $static)`, then `$field` will be set to `$static`.
/// If `$fieldty` is `option`, then `$field` is optional field.
/// If `$fieldty` is `upgradable_option`, then `$field` is optional and read via [`MaybeReadable`].
/// If `$fieldty` is `upgradable_required`, then `$field` is stored as an [`Option`] and read via
///    [`MaybeReadable`], requiring the TLV to be present.
/// If `$fieldty` is `optional_vec`, then `$field` is a [`Vec`], which needs to have its individual elements serialized.
///    Note that for `optional_vec` no bytes are written if the vec is empty
/// If `$fieldty` is `(legacy, $ty, $write)` then, when writing, the function $write will be
///    called with the object being serialized and a returned `Option` and is written as a TLV if
///    `Some`. When reading, an optional field of type `$ty` is read (which can be used in later
///    `default_value` or `static_value` fields by referring to the value by name).
///
/// For example,
/// ```
/// struct LightningMessage {
///     tlv_integer: u32,
///     tlv_default_integer: u32,
///     tlv_optional_integer: Option<u32>,
///     tlv_vec_type_integer: Vec<u32>,
///        tlv_upgraded_integer: u32,
/// }
///
/// ```
///
/// [`Readable`]: crate::util::ser::Readable
/// [`MaybeReadable`]: crate::util::ser::MaybeReadable
/// [`Writeable`]: crate::util::ser::Writeable
/// [`Vec`]: crate::prelude::Vec
#[macro_export]
macro_rules! impl_writeable_tlv_based {
    ($st: ident, {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}) => {
        impl $crate::util::ser::Writeable for $st {
            fn write<W: $crate::util::ser::Writer>(&self, writer: &mut W) -> Result<(), $crate::io::Error> {
                $crate::_encode_varint_length_prefixed_tlv!(writer, {
                    $(($type, &self.$field, $fieldty, self)),*
                });
                Ok(())
            }

            #[inline]
            fn serialized_length(&self) -> usize {
                use $crate::util::ser::BigSize;
                let len = {
                    #[allow(unused_mut)]
                    let mut len = $crate::util::ser::LengthCalculatingWriter(0);
                    $(
                        $crate::_get_varint_length_prefixed_tlv_length!(len, $type, &self.$field, $fieldty, self);
                    )*
                    len.0
                };
                let mut len_calc = $crate::util::ser::LengthCalculatingWriter(0);
                BigSize(len as u64).write(&mut len_calc).expect("No in-memory data may fail to serialize");
                len + len_calc.0
            }
        }

        impl $crate::util::ser::Readable for $st {
            fn read<R: $crate::io::Read>(reader: &mut R) -> Result<Self, $crate::ln::msgs::DecodeError> {
                Ok($crate::_decode_and_build!(reader, Self, {$(($type, $field, $fieldty)),*}))
            }
        }
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! _impl_writeable_tlv_based_enum_common {
    ($st: ident, $(($variant_id: expr, $variant_name: ident) =>
        {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}
    ),* $(,)?;
    // $tuple_variant_* are only passed from `impl_writeable_tlv_based_enum_*_legacy`
    $(($tuple_variant_id: expr, $tuple_variant_name: ident)),* $(,)?;
    // $length_prefixed_* are only passed from `impl_writeable_tlv_based_enum_*` non-`legacy`
    $(($length_prefixed_tuple_variant_id: expr, $length_prefixed_tuple_variant_name: ident)),* $(,)?) => {
        impl $crate::util::ser::Writeable for $st {
            fn write<W: $crate::util::ser::Writer>(&self, writer: &mut W) -> Result<(), $crate::io::Error> {
                lightning_macros::skip_legacy_fields!(match self {
                    $($st::$variant_name { $(ref $field: $fieldty, )* .. } => {
                        let id: u8 = $variant_id;
                        id.write(writer)?;
                        $crate::_encode_varint_length_prefixed_tlv!(writer, {
                            $(($type, $field, $fieldty, self)),*
                        });
                    }),*
                    $($st::$tuple_variant_name (ref field) => {
                        let id: u8 = $tuple_variant_id;
                        id.write(writer)?;
                        field.write(writer)?;
                    }),*
                    $($st::$length_prefixed_tuple_variant_name (ref field) => {
                        let id: u8 = $length_prefixed_tuple_variant_id;
                        id.write(writer)?;
                        $crate::util::ser::BigSize(field.serialized_length() as u64).write(writer)?;
                        field.write(writer)?;
                    }),*
                });
                Ok(())
            }
        }
    }
}

/// Implement [`Readable`] and [`Writeable`] for an enum, with struct variants stored as TLVs and tuple
/// variants stored directly.
///
/// The format is, for example,
/// ```
/// enum EnumName {
///   StructVariantA {
///     required_variant_field: u64,
///     optional_variant_field: Option<u8>,
///   },
///   StructVariantB {
///     variant_field_a: bool,
///     variant_field_b: u32,
///     variant_vec_field: Vec<u32>,
///   },
///   TupleVariantA(),
///   TupleVariantB(Vec<u8>),
/// }
/// ```
///
/// The type is written as a single byte, followed by length-prefixed variant data.
///
/// Attempts to read an unknown type byte result in [`DecodeError::UnknownRequiredFeature`].
///
/// Note that the serialization for tuple variants (as well as the call format) was changed in LDK
/// 0.0.124.
///
/// [`Readable`]: crate::util::ser::Readable
/// [`Writeable`]: crate::util::ser::Writeable
/// [`DecodeError::UnknownRequiredFeature`]: crate::ln::msgs::DecodeError::UnknownRequiredFeature
#[macro_export]
macro_rules! impl_writeable_tlv_based_enum {
    ($st: ident,
        $(($variant_id: expr, $variant_name: ident) =>
            {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}
        ),*
        $($(,)? {$tuple_variant_id: expr, $tuple_variant_name: ident} => ()),*
        $(,)?
    ) => {
        $crate::_impl_writeable_tlv_based_enum_common!($st,
            $(($variant_id, $variant_name) => {$(($type, $field, $fieldty)),*}),*
            ;;
            $(($tuple_variant_id, $tuple_variant_name)),*);

        impl $crate::util::ser::Readable for $st {
            #[allow(unused_mut)]
            fn read<R: $crate::io::Read>(mut reader: &mut R) -> Result<Self, $crate::ln::msgs::DecodeError> {
                let id: u8 = $crate::util::ser::Readable::read(reader)?;
                match id {
                    $($variant_id => {
                        // Because read_tlv_fields creates a labeled loop, we cannot call it twice
                        // in the same function body. Instead, we define a closure and call it.
                        let mut f = || {
                            Ok($crate::_decode_and_build!(reader, $st::$variant_name, {$(($type, $field, $fieldty)),*}))
                        };
                        f()
                    }),*
                    $($tuple_variant_id => {
                        let length: $crate::util::ser::BigSize = $crate::util::ser::Readable::read(reader)?;
                        let mut s = $crate::util::ser::FixedLengthReader::new(reader, length.0);
                        let res = $crate::util::ser::LengthReadable::read_from_fixed_length_buffer(&mut s)?;
                        if s.bytes_remain() {
                            s.eat_remaining()?; // Return ShortRead if there's actually not enough bytes
                            return Err($crate::ln::msgs::DecodeError::InvalidValue);
                        }
                        Ok($st::$tuple_variant_name(res))
                    }),*
                    _ => {
                        Err($crate::ln::msgs::DecodeError::UnknownRequiredFeature)
                    },
                }
            }
        }
    }
}

/// Implement [`MaybeReadable`] and [`Writeable`] for an enum, with struct variants stored as TLVs and
/// tuple variants stored directly.
///
/// This is largely identical to [`impl_writeable_tlv_based_enum`], except that odd variants will
/// return `Ok(None)` instead of `Err(`[`DecodeError::UnknownRequiredFeature`]`)`. It should generally be preferred
/// when [`MaybeReadable`] is practical instead of just [`Readable`] as it provides an upgrade path for
/// new variants to be added which are simply ignored by existing clients.
///
/// Note that the serialization for tuple variants (as well as the call format) was changed in LDK
/// 0.0.124.
///
/// [`MaybeReadable`]: crate::util::ser::MaybeReadable
/// [`Writeable`]: crate::util::ser::Writeable
/// [`DecodeError::UnknownRequiredFeature`]: crate::ln::msgs::DecodeError::UnknownRequiredFeature
/// [`Readable`]: crate::util::ser::Readable
#[macro_export]
macro_rules! impl_writeable_tlv_based_enum_upgradable {
    ($st: ident,
        $(($variant_id: expr, $variant_name: ident) =>
            {$(($type: expr, $field: ident, $fieldty: tt)),* $(,)*}
        ),*
        $(, {$tuple_variant_id: expr, $tuple_variant_name: ident} => ())*
        $(, unread_variants: $($unread_variant: ident),*)?
        $(,)?
    ) => {
        $crate::_impl_writeable_tlv_based_enum_common!($st,
            $(($variant_id, $variant_name) => {$(($type, $field, $fieldty)),*}),*
            $(, $((255, $unread_variant) => {}),*)?
            ;;
            $(($tuple_variant_id, $tuple_variant_name)),*);

        impl $crate::util::ser::MaybeReadable for $st {
            #[allow(unused_mut)]
            fn read<R: $crate::io::Read>(mut reader: &mut R) -> Result<Option<Self>, $crate::ln::msgs::DecodeError> {
                let id: u8 = $crate::util::ser::Readable::read(reader)?;
                match id {
                    $($variant_id => {
                        // Because read_tlv_fields creates a labeled loop, we cannot call it twice
                        // in the same function body. Instead, we define a closure and call it.
                        let mut f = || {
                            Ok(Some($crate::_decode_and_build!(reader, $st::$variant_name, {$(($type, $field, $fieldty)),*})))
                        };
                        f()
                    }),*
                    $($tuple_variant_id => {
                        let length: $crate::util::ser::BigSize = $crate::util::ser::Readable::read(reader)?;
                        let mut s = $crate::util::ser::FixedLengthReader::new(reader, length.0);
                        let res = $crate::util::ser::Readable::read(&mut s)?;
                        if s.bytes_remain() {
                            s.eat_remaining()?; // Return ShortRead if there's actually not enough bytes
                            return Err($crate::ln::msgs::DecodeError::InvalidValue);
                        }
                        Ok(Some($st::$tuple_variant_name(res)))
                    }),*
                    // Note that we explicitly match 255 here to reserve it for use in
                    // `unread_variants`.
                    255|_ if id % 2 == 1 => {
                        let tlv_len: $crate::util::ser::BigSize = $crate::util::ser::Readable::read(reader)?;
                        let mut rd = $crate::util::ser::FixedLengthReader::new(reader, tlv_len.0);
                        rd.eat_remaining().map_err(|_| $crate::ln::msgs::DecodeError::ShortRead)?;
                        Ok(None)
                    },
                    _ => Err($crate::ln::msgs::DecodeError::UnknownRequiredFeature),
                }
            }
        }
    }
}
