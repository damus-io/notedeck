use std::{marker::PhantomData, process::Command};

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
        if let Ok(status) = Command::new("xdg-open").arg(self.inner).status() {
            if status.success() {
                return Ok(());
            }
        }
        Err(Error::Unknown)
    }
}
