#!/usr/bin/env bash
# Cross-build Temino for Intel Haiku from the Mac and package it:
#   haiku/cross.sh x86_64      -> dist/temino-<v>-1-x86_64.hpkg
#   haiku/cross.sh x86         -> dist/temino-<v>-1-x86_gcc2.hpkg (32-bit,
#                                 built for the gcc 13 secondary arch)
# Compiles in Haiku's cross-compiler image (haiku/cross/Dockerfile adds Rust
# nightly; the targets are tier 3, so std is built with -Zbuild-std), then
# packages on the Haiku box (HAIKU_HOST, default haiku-dbg): rc, xres and
# mimeset are Haiku's own tools, and they handle any architecture's binary.
# DOCKER_CONTEXT picks the Docker engine (amd64 images; Rosetta on Apple
# silicon).
set -euo pipefail
cd "$(dirname "$0")/.."
realign=()

case "${1:-}" in
	x86_64) base=x86_64-r1beta5; triple=x86_64-unknown-haiku; pkgarch=x86_64; tc=x86_64-unknown-haiku; sfx= ;;
	# 32-bit Haiku only guarantees a 4-byte-aligned stack, but LLVM and gcc
	# assume 16 on i686, so SSE spills (movdqa) to the stack fault; realign
	# wherever a function needs it (rustls' crypto crashed without this).
	x86) base=x86_gcc2h-r1beta5; triple=i686-unknown-haiku; pkgarch=x86_gcc2; tc=i586-pc-haiku; sfx=-x86
		realign=(-e "RUSTFLAGS=-C llvm-args=-stackrealign" -e "CFLAGS_i686_unknown_haiku=-mstackrealign" -e "CXXFLAGS_i686_unknown_haiku=-mstackrealign") ;;
	*) echo "usage: haiku/cross.sh x86_64|x86" >&2; exit 2 ;;
esac
image=haiku-rust-cross:${1}
host=${HAIKU_HOST:-haiku-dbg}
env=${triple//-/_}
ENV=$(echo "$env" | tr a-z A-Z)

echo ">> image $image"
docker build -q --platform linux/amd64 --build-arg BASE=ghcr.io/haiku/cross-compiler:$base \
	-t "$image" haiku/cross >/dev/null

echo ">> cargo build --target $triple"
docker volume create haiku-cross-cargo >/dev/null
docker run --rm --platform linux/amd64 -v "$PWD":/src -v haiku-cross-cargo:/opt/cargo/registry -w /src \
	-e CARGO_TARGET_DIR=/src/target/cross \
	-e "CARGO_TARGET_${ENV}_LINKER=$tc-gcc$sfx" \
	-e "CC_$env=$tc-gcc$sfx" -e "CXX_$env=$tc-g++$sfx" -e "AR_$env=$tc-ar$sfx" \
	${realign[@]+"${realign[@]}"} \
	"$image" cargo build --release --locked -Zbuild-std=std,panic_abort --target "$triple"
bin=target/cross/$triple/release/temino

echo ">> packaging for $pkgarch on $host"
src='~/src/temino-cross'
COPYFILE_DISABLE=1 tar czf - --no-xattrs haiku README.md LICENSE Cargo.toml assets/fonts/JetBrainsMono-OFL.txt "$bin" \
	| ssh "$host" "rm -rf $src && mkdir -p $src && cd $src && tar xzf -"
pkg=$(ssh "$host" "cd $src && TEMINO_PKG_REV=${TEMINO_PKG_REV:-1} TEMINO_BIN=$bin TEMINO_ARCH=$pkgarch haiku/package.sh" | tail -1)
mkdir -p dist
scp -q "$host:$src/$pkg" dist/
echo ">> dist/$(basename "$pkg")"
