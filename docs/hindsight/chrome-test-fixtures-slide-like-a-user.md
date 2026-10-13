# Chrome test fixtures navigate with the user's default slide, not with it off

Headless chrome fixtures turn `animate_nav_transitions` off, so nothing they
drive ever lands mid-slide; a bug that needs the slide ships past them.

- headway:notedeck/age-empower-gun — the epic-review macro test ran with slides off, so a macro step firing mid Headway→Headway slide passed and was sent back from a hand test; another release build of the chrome test to reproduce it
