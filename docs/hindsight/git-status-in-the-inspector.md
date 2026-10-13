# The session's git status lives in the inspector

Every other kind-31990 source is the inspector's, but GitStatusCache sits on
AgenticSessionData, so snapshot, snapshot_key and restore each pass git on the side.

- headway:dave/during-similar-scheme — ~5 min: threaded `&GitStatusCache` into snapshot_key/snapshot and made restore return the tree for fold_snapshot to place; moving the cache into SessionInspector was blocked by build_dave_ui borrowing the inspector and git_status mutably side by side
