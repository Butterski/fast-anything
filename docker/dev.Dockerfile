# Build and test image for this fork: g++ and the Python headers for C++ ports, NumPy for
# NumPy ports, Rust for the CLI itself. Ubuntu 26.04 matches the development host, so a
# fast-anything binary built here runs there.
#
#   docker build -t fast-anything-dev -f docker/dev.Dockerfile docker
#   docker/dev.sh cargo test
FROM ubuntu:26.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential python3-dev python3-numpy git curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
      --component clippy --component rustfmt \
 && chmod -R a+rwX /opt/cargo /opt/rustup
