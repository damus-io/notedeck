# notedeck_dave's lib tests are red on the tip

Four dave lib tests fail before any change (a spec runner's remote stop, two ui
status-line tests, a flaky fan-out), so each card reruns them on a stash.

- headway:dave/talk-giraffe-south — three extra full `cargo test -p notedeck_dave --lib` runs on a stash and in isolation to show all four failed on 530360d51125 too, and that poll_fans_out_only_what_the_host_did_not_publish only fails under load
- headway:dave/brisk-eight-culture — a WIP commit and a full dave lib run on its parent to show the two ui status-line tests were red before the change
- headway:dave/cancel-limit-thrive — read the hindsight file to learn a_remote_stop_of_a_spec_session_reaches_its_runner was already red, instead of a stash rerun
- headway:ux-feedback/typical-ice-renew — ~3 min: a WIP commit and a rerun on its parent to show the_chat_heads_and_foots_its_turns and a_running_turn_shows_its_time_on_the_status_line were red before the change; should have read this file first
- headway:ux-feedback/venture-lock-now — read this file first and skipped the rerun: the same two ui status-line tests were the only red ones
