# One struct for the Dave-level inputs every session view reads

build_dave_ui takes Dave's state as loose parameters that three layouts and a
harness each pass through, so one new account-wide value touches five signatures.

- headway:dave/tray-ghost-view — ~4 min: threaded the account's rate limits through build_dave_ui, scene_ui, desktop_ui, narrow_ui, view.rs's three calls and SessionViewState; wanted a DaveUiInputs { model_config, run_configs, running_sessions, rate_limit, … } borrowed once from Dave
