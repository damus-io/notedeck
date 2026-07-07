# Lavapipe (software Vulkan) environment for snapshot tests, mirroring the
# CI snapshot job (.github/workflows/rust.yml, snapshot-test): ubuntu-22.04,
# stable rust, and the same apt dependencies plus mesa-vulkan-drivers.
#
# Used automatically by scripts/snapshot-test on macOS when updating
# snapshots (--update), where no software Vulkan driver exists — the script
# builds this image once, then keeps a per-checkout container running from
# it. CI does not use this file; it runs lavapipe natively.
FROM ubuntu:22.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git pkg-config \
        libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
        libspeechd-dev libxkbcommon-dev libssl-dev libasound2-dev \
        mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/*

# The toolchain is baked into the image under /opt. The runtime CARGO_HOME
# (/cargo, registry cache) and CARGO_TARGET_DIR (/target, builds) are set to
# volume mount points so they persist across container recreation; rustup's
# cargo shim finds the real toolchain via RUSTUP_HOME regardless of CARGO_HOME.
ENV RUSTUP_HOME=/opt/rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | CARGO_HOME=/opt/cargo sh -s -- -y --default-toolchain stable --profile minimal

ENV PATH=/opt/cargo/bin:$PATH \
    CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target

WORKDIR /repo
CMD ["sleep", "infinity"]
