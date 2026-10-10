# Drive Dave's update loop from a test without building an AppContext

restore, poll, dispatch and key handling live on Dave methods that take an
AppContext, so each test suite extracts a ctx struct or steals a SessionManager.

- headway:dave/antenna-resource-edit — the end-of-turn reconcile had to be extracted and the deliver test swaps SessionManager in and out of a test_dave; wanted a SessionsCtx { sessions, ndb, sk, author }
- headway:dave/ethics-code-bread — invented DispatchCtx beside ApplyCtx where one HostCtx { ndb, secret_key, waker } would do; update.rs's dispatch_turn test helper still hand-copies stream_request
- headway:dave/year-retire-voyage — Host::restart stands up a throwaway Dave to steal its session_manager because restore lives on Dave, not on a SessionManager
- headway:headway/left-own-demand — wrote test_host() in restore.rs tests (Notedeck::init --testrunner + select account); pns_outbox_e2e and chip_click tests each hand-roll the same
- headway:notedeck/use-release-oyster — Dispatch in ui/mod.rs tests owns loose fields rather than a Dave, so the UI-tool tests built a second fixture, and test_dave quietly opens on the DirectoryPicker
- headway:dave/okay-scale-wrap — hand-rolled SessionViewState in ui/mod.rs tests (Notedeck::init --testrunner, then build_dave_ui and handle_ui_action) just to render one DaveUi and route its clicks
- headway:dave/amateur-fuel-stomach — the session-list chip test re-rolled Notedeck::init --testrunner + add/select account + a kind-31988 seed copied from chip_click_tests, for want of a shared test host
