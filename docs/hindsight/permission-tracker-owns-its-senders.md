# The permission tracker owns its senders

Card: headway:notedeck/account-interest-title
`PermissionTracker::pending` is public, so a caller can answer a request past
`resolve`, and every per-answer duty is repeated at each site that does.

- headway:dave/girl-actress-shoot — wake-on-answer meant auditing five sites that touch `pending` and rerouting the remote path's direct `send` through a new `PermissionTracker::answer`; private fields would have made the one funnel the only way in
