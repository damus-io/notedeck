# A Dave session is created from one struct

Card: headway:notedeck/final-ship-dish
Fresh creates take a whole NewSession; resumed creates, placeholders, restore and
test sites still set profile/spec after creation, so a forgetful path drops them.

- headway:dave/hurt-tortoise-exact — the backend `profile` slug couldn't be threaded through ~108 constructor calls, so it is assigned after creation (clone_session had to copy it by hand, or a clone would run on the plain backend)
- headway:dave/keen-enrich-protect — the profile is still set after creation in four more places (the picker's create, the resumed-session create, the 31989 spawn handler, the worktree creator); a BackendChoice { backend, profile, model } now carries it to Dave's create, but update::create_session_with_cwd still takes backend and model apart
- headway:dave/picnic-diagram-dizzy — the agent `spec` slug took the same after-creation path as `profile`: set by hand in apply_spawn_command and clone_session, then restore, publish and the kind-31988 tag beside it; ~13 sites for one session attribute, and the remote clone (a BackendChoice without it) still drops it
- headway:dave/decade-finish-toddler — `spec` joined `BackendChoice`, but every literal of it (picker, worktree creator, clone, 3 tests) took a `spec: None` by hand, and Dave::create_session_with_cwd and the worktree creator still set it on the session after creation; one struct handed to update::create_session_with_cwd would have been one site
- headway:notedeck/final-ship-dish — built `NewSession`; the resumed create and placeholders stayed positional because `new_resumed`/`new_pending_placeholder` each take a different extra (resume id + title, spawn id + host), so one struct for every create needs a `Start` enum on it
