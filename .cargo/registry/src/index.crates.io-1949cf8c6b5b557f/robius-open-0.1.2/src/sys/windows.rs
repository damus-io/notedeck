use std::marker::PhantomData;
use crate::{Result, Error};

use windows::{
    core::HSTRING,
    Foundation,
    System::Launcher,
};


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
        let win_uri = Foundation::Uri::CreateUri(&HSTRING::from(self.inner))
            .map_err(|_| Error::MalformedUri)?;

        match Launcher::LaunchUriAsync(&win_uri)
            .map_err(|_| crate::Error::Unknown)?
            .get()
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(Error::NoHandler),
            Err(_) => Err(Error::Unknown)
        }
    }
}
