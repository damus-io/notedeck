# ChromeDriver lends the chrome and an AppContext together

A test calling a chrome method that takes an `AppContext` splits the driver's
borrow by hand, and through a fixture that derefs to the driver it won't compile.

- headway:notedeck/practice-crash-blouse — porting tests/common onto ChromeDriver behind a Deref broke `f.chrome.x(&mut f.notedeck.app_context())` in key_hints, user_keymap and keymap_macros; each needed its own destructure
