# A kittest harness can hold an app's update back

`Harness::builder().build_*` runs frames until the app settles, so an app
whose first `update` starts async work has finished it before a test can look
at the frame in between; each suite needs its own switch to see it.

- headway:cashflow/crash-area-planet — ~10 min: the loading pane couldn't be snapshotted or asserted until `CashflowTestState::hold_updates` built the harness with `update` skipped, then released it for one `step`
