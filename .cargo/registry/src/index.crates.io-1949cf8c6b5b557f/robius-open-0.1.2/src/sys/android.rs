use jni::objects::JValueGen;

use crate::{Error, Result};

pub(crate) struct Uri<'a, 'b> {
    inner: &'a str,
    action: &'b str,
}

impl<'a, 'b> Uri<'a, 'b> {
    pub(crate) fn new(inner: &'a str) -> Self {
        Self {
            inner,
            action: "ACTION_VIEW",
        }
    }

    pub(crate) fn action(self, action: &'b str) -> Self {
        Self { action, ..self }
    }

    pub(crate) fn open(self) -> Result<()> {
        let res = robius_android_env::with_activity(|env, current_activity| {
            let action = env
                .get_static_field("android/content/Intent", self.action, "Ljava/lang/String;")?
                .l()?;

            let string = env
                .new_string(self.inner)
                .map_err(|_| Error::MalformedUri)?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValueGen::Object(&string)],
                )?
                .l()?;

            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValueGen::Object(&action), JValueGen::Object(&uri)],
            )?;

            #[cfg(feature = "android-result")]
            let is_err = {
                let package_manager = env
                    .call_method(
                        current_activity,
                        "getPackageManager",
                        "()Landroid/content/pm/PackageManager;",
                        &[],
                    )?
                    .l()?;

                let component_name = env
                    .call_method(
                        &intent,
                        "resolveActivity",
                        "(Landroid/content/pm/PackageManager;)Landroid/content/ComponentName;",
                        &[JValueGen::Object(&package_manager)],
                    )?
                    .l()?;

                component_name.as_raw().is_null()
            };
            #[cfg(not(feature = "android-result"))]
            let is_err = false;

            if is_err {
                // NOTE: If the correct permissions aren't added to the app manifest,
                // resolveActivity will return null regardless.
                Err(Error::NoHandler)
            } else {
                env.call_method(
                    current_activity,
                    "startActivity",
                    "(Landroid/content/Intent;)V",
                    &[JValueGen::Object(&intent)],
                )?;
                Ok(())
            }
        });

        match res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                #[cfg(feature = "log")]
                log::error!(
                    "resolveActivity method failed. Is your app manifest missing permissions?"
                );
                Err(e)
            }
            Err(_e) => {
                #[cfg(feature = "log")]
                log::error!(
                    "Couldn't get current activity or JVM/JNI. Did you set up `robius_android_env` correctly?"
                );
                Err(Error::AndroidEnvironment)
            }
        }
    }
}
