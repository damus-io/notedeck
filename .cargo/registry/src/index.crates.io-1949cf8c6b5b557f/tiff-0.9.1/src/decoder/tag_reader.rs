use std::convert::TryFrom;
use std::io::{Read, Seek};

use crate::tags::Tag;
use crate::{TiffError, TiffFormatError, TiffResult};

use super::ifd::{Directory, Value};
use super::stream::SmartReader;
use super::Limits;

pub(crate) struct TagReader<'a, R: Read + Seek> {
    pub reader: &'a mut SmartReader<R>,
    pub ifd: &'a Directory,
    pub limits: &'a Limits,
    pub bigtiff: bool,
}
impl<'a, R: Read + Seek> TagReader<'a, R> {
    pub(crate) fn find_tag(&mut self, tag: Tag) -> TiffResult<Option<Value>> {
        Ok(match self.ifd.get(&tag) {
            Some(entry) => Some(entry.clone().val(self.limits, self.bigtiff, self.reader)?),
            None => None,
        })
    }
    pub(crate) fn require_tag(&mut self, tag: Tag) -> TiffResult<Value> {
        match self.find_tag(tag)? {
            Some(val) => Ok(val),
            None => Err(TiffError::FormatError(
                TiffFormatError::RequiredTagNotFound(tag),
            )),
        }
    }
    pub fn find_tag_uint_vec<T: TryFrom<u64>>(&mut self, tag: Tag) -> TiffResult<Option<Vec<T>>> {
        self.find_tag(tag)?
            .map(|v| v.into_u64_vec())
            .transpose()?
            .map(|v| {
                v.into_iter()
                    .map(|u| {
                        T::try_from(u).map_err(|_| TiffFormatError::InvalidTagValueType(tag).into())
                    })
                    .collect()
            })
            .transpose()
    }
}
