use std::marker::PhantomData;

use icrate::{
    AppKit::NSWorkspace,
    Foundation::{NSString, NSURL},
};

use crate::{Error, Result};

pub(crate) struct Uri<'a, 'b> {
    inner: &'a str,
    phantom: PhantomData<&'b ()>,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(inner: &'a str) -> Self {
        Self {
            inner,
            phantom: PhantomData,
        }
    }

    pub fn action(self, _: &'b str) -> Self {
        self
    }

    pub fn open(self) -> Result<()> {
        let string = NSString::from_str(self.inner);
        let url = unsafe { NSURL::URLWithString(&string) }.ok_or(Error::MalformedUri)?;
        let workspace = unsafe { NSWorkspace::sharedWorkspace() };

        if unsafe { workspace.openURL(&url) } {
            Ok(())
        } else {
            Err(Error::Unknown)
        }
    }
}
