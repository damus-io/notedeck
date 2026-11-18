# A Dave session is created from one struct

Session constructors take their settings positionally at ~108 call sites, so a
new one is set after creation, and a path that forgets it drops it silently.

- headway:dave/hurt-tortoise-exact — the backend `profile` slug couldn't be threaded through ~108 constructor calls, so it is assigned after creation (clone_session had to copy it by hand, or a clone would run on the plain backend)
- headway:dave/keen-enrich-protect — the profile is still set after creation in four more places (the picker's create, the resumed-session create, the 31989 spawn handler, the worktree creator); a BackendChoice { backend, profile, model } now carries it to Dave's create, but update::create_session_with_cwd still takes backend and model apart
- headway:dave/picnic-diagram-dizzy — the agent `spec` slug took the same after-creation path as `profile`: set by hand in apply_spawn_command and clone_session, then restore, publish and the kind-31988 tag beside it; ~13 sites for one session attribute, and the remote clone (a BackendChoice without it) still drops it
