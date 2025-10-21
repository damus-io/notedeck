use std::marker::PhantomData;

use crate::{Error, Result};

pub(crate) struct Uri<'a, 'b> {
    phantom: PhantomData<(&'a (), &'b ())>,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(_: &'a str) -> Self {
        Self {
            phantom: PhantomData,
        }
    }

    pub fn action(self, _: &'b str) -> Self {
        self
    }

    pub fn open(self) -> Result<()> {
        Err(Error::Unknown)
    }
}
