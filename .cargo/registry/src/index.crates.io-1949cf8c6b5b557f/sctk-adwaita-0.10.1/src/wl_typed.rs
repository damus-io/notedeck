use std::{marker::PhantomData, ops::Deref};

use smithay_client_toolkit::reexports::client::{Dispatch, Proxy};

#[derive(Debug)]
pub struct WlTyped<I, DATA>(I, PhantomData<DATA>);

impl<I, DATA> WlTyped<I, DATA>
where
    I: Proxy,
    DATA: Send + Sync + 'static,
{
    #[allow(clippy::extra_unused_type_parameters)]
    pub fn wrap<STATE>(i: I) -> Self
    where
        STATE: Dispatch<I, DATA>,
    {
        Self(i, PhantomData)
    }

    pub fn inner(&self) -> &I {
        &self.0
    }

    #[allow(dead_code)]
    pub fn data(&self) -> &DATA {
        // Generic on Self::wrap makes sure that this will never panic
        #[allow(clippy::unwrap_used)]
        self.0.data().unwrap()
    }
}

impl<I: Clone, D> Clone for WlTyped<I, D> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

impl<I, D> Deref for WlTyped<I, D> {
    type Target = I;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<I: PartialEq, D> PartialEq for WlTyped<I, D> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<I: Eq, D> Eq for WlTyped<I, D> {}
