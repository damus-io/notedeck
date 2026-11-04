# A Dave session is created from one struct

Session constructors take their settings positionally at ~108 call sites, so a
new one is set after creation, and a path that forgets it drops it silently.

- headway:dave/hurt-tortoise-exact — the backend `profile` slug couldn't be threaded through ~108 constructor calls, so it is assigned after creation (clone_session had to copy it by hand, or a clone would run on the plain backend)
