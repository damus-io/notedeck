use std::marker::PhantomData;

use icrate::{
    block2::{Block, ConcreteBlock},
    Foundation::{NSDictionary, NSString, NSURL},
};
use objc2::{
    extern_class, extern_methods, mutability,
    rc::Id,
    runtime::{AnyObject, Bool, NSObject},
    ClassType,
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

        let application = unsafe { UIApplication::shared() };
        let (tx, rx) = std::sync::mpsc::channel();
        let block = ConcreteBlock::new(move |success| {
            // NOTE: We want to panic here as the main thread will hang waiting for a
            // message on the channel.
            tx.send(success).expect("failed to send open result");
        })
        .copy();

        application.open(&url, &NSDictionary::new(), &block);

        match rx.recv() {
            Ok(success) if success.is_true() => Ok(()),
            _ => Err(Error::Unknown),
        }
    }
}

extern_class!(
    struct UIResponder;

    unsafe impl ClassType for UIResponder {
        type Super = NSObject;
        // TODO: Can this be relaxed?
        type Mutability = mutability::InteriorMutable;
    }
);

extern_class!(
    struct UIApplication;

    unsafe impl ClassType for UIApplication {
        type Super = UIResponder;
        // TODO: Can this be relaxed?
        type Mutability = mutability::InteriorMutable;
    }
);

extern_methods!(
    unsafe impl UIApplication {
        #[method_id(sharedApplication)]
        pub unsafe fn shared() -> Id<UIApplication>;

        #[method(canOpenURL:)]
        fn can_open(&self, url: &NSURL) -> bool;

        #[method(openURL:options:completionHandler:)]
        fn open(
            &self,
            url: &NSURL,
            // TODO?
            options: &NSDictionary<NSString, AnyObject>,
            // TODO?
            completion_handler: &Block<(Bool,), ()>,
        );
    }
);
