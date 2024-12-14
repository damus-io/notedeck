//#[cfg(target_os = "android")]
//use egui_android::run_android;

use crate::app::Notedeck;
use notedeck_columns::Damus;
use winit::platform::android::activity::AndroidApp;
use winit::platform::android::EventLoopBuilderExtAndroid;

#[no_mangle]
#[tokio::main]
pub async fn android_main(app: AndroidApp) {
    std::env::set_var("RUST_BACKTRACE", "full");
    android_logger::init_once(android_logger::Config::default().with_min_level(log::Level::Info));

    let path = app.internal_data_path().expect("data path");
    let mut options = eframe::NativeOptions::default();
    options.renderer = eframe::Renderer::Wgpu;
    // Clone `app` to use it both in the closure and later in the function
    let app_clone_for_event_loop = app.clone();
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app_clone_for_event_loop);
    }));

    let app_args = get_app_args(app);

    let _res = eframe::run_native(
        "Damus Notedeck",
        options,
        Box::new(move |cc| {
            let mut notedeck = Notedeck::new(&cc.egui_ctx, path, &app_args);
            let damus = Damus::new(&mut notedeck.app_context(), &app_args);
            notedeck.add_app(damus);
            Ok(Box::new(notedeck))
        }),
    );
}

use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/*
Read args from a config file:
- allows use of more interesting args w/o risk of checking them in by mistake
- allows use of different args w/o rebuilding the app
- uses compiled in defaults if config file missing or broken

Example android-config.json:
```
{
  "args": [
    "--npub",
    "npub1h50pnxqw9jg7dhr906fvy4mze2yzawf895jhnc3p7qmljdugm6gsrurqev",
    "-c",
    "contacts",
    "-c",
    "notifications"
  ]
}
```

Install/update android-config.json with:
```
adb push android-config.json /sdcard/Android/data/com.damus.app/files/android-config.json
```

Using internal storage would be better but it seems hard to get the config file onto
the device ...
*/

fn get_app_args(app: AndroidApp) -> Vec<String> {
    let external_data_path: PathBuf = app
        .external_data_path()
        .expect("external data path")
        .to_path_buf();
    let config_file = external_data_path.join("android-config.json");

    let default_args = vec![
        "--pub",
        "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245",
        "-c",
        "contacts",
        "-c",
        "notifications",
        "-c",
        "notifications:3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect();

    if config_file.exists() {
        if let Ok(config_contents) = fs::read_to_string(config_file) {
            if let Ok(json) = serde_json::from_str::<Value>(&config_contents) {
                if let Some(args_array) = json.get("args").and_then(|v| v.as_array()) {
                    let config_args = args_array
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();

                    return config_args;
                }
            }
        }
    }

    default_args // Return the default args if config is missing or invalid
}
