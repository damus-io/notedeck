# A kittest harness can hold an app's update back

`build_*` runs frames until the app settles, so async work an app's first `update`
starts is done before a test sees the frame between; each suite adds its own switch.

- headway:cashflow/crash-area-planet — ~10 min: the loading pane couldn't be snapshotted or asserted until `CashflowTestState::hold_updates` built the harness with `update` skipped, then released it for one `step`
