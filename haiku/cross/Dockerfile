# Haiku's own cross-compiler image (gcc 13 + an R1/beta5 sysroot + the
# `package` tool) plus Rust nightly with rust-src, for -Zbuild-std: the Haiku
# targets are tier 3, so rustup ships no prebuilt std for them.
#   BASE=ghcr.io/haiku/cross-compiler:x86_64-r1beta5    -> x86_64-unknown-haiku
#   BASE=ghcr.io/haiku/cross-compiler:x86_gcc2h-r1beta5 -> i686-unknown-haiku
ARG BASE
FROM ${BASE}
ARG RUST_TOOLCHAIN=nightly
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_TOOLCHAIN} \
 && rustup component add rust-src
