# agentium-sandbox lifecycle tests run without KVM

A fake microsandbox backend, so guest start/stop/reap/sweep run in every CI job.
Without one they only run under /dev/kvm, in the opt-in #[ignore] boot suite.

- headway:dave/gadget-flavor-ghost — ~5 min: split the reap rules into a pure `verdict` to get any coverage, and left start/shutdown/drop/sweep proven only by type-check plus two ignored boot tests
