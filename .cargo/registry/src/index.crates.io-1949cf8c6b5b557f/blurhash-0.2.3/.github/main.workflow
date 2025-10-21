workflow "Test and Publish" {
  on = "push"
  resolves = ["Release"]
}

action "Fmt Check and Test" {
  uses = "icepuma/rust-action@master"
  args = "cargo fmt -- --check && cargo clippy -- -Dwarnings && cargo test"
}

action "Tag" {
  uses = "actions/bin/filter@master"
  needs = ["Fmt Check and Test"]
  args = "tag v*"
}

action "Release" {
  uses = "icepuma/rust-action@master"
  needs = ["Tag"]
  args = "cargo login $CARGO_TOKEN && cargo publish"
  secrets = ["CARGO_TOKEN"]
}
