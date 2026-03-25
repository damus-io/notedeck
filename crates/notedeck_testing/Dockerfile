FROM ubuntu:22.04

RUN apt-get update && apt-get install -y \
    curl build-essential pkg-config git ca-certificates \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libspeechd-dev libxkbcommon-dev libssl-dev && \
    rm -rf /var/lib/apt/lists/*

RUN curl https://sh.rustup.rs -sSf | sh -s -- -y
ENV PATH=/root/.cargo/bin:$PATH
