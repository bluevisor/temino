#!/bin/bash
# Builds Temino and wraps it in an .hpkg. Run ON Haiku:
#   haiku/package.sh                     -> dist/temino-<version>-<rev>-<arch>.hpkg
#   TEMINO_PKG_REV=2 haiku/package.sh     bump the package revision
# Install by copying the .hpkg into ~/config/packages (activates at once).
# Temino needs a Vulkan driver: libvulkan.so.1 on the library path (on the
# UTM VM, the Venus stack in ~/config/non-packaged/lib).
set -eo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rev=${TEMINO_PKG_REV:-1}
arch=$(getarch)
out="dist/temino-$version-$rev-$arch.hpkg"

cargo build --release

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/apps" "$stage/bin" "$stage/data/deskbar/menu/Applications" \
	"$stage/documentation/packages/temino"

app="$stage/apps/Temino"
install -m 755 target/release/temino "$app"
rc -o "$stage/Temino.rsrc" haiku/Temino.rdef
xres -o "$app" "$stage/Temino.rsrc"
rm "$stage/Temino.rsrc"
mimeset -f "$app"
ln -s ../apps/Temino "$stage/bin/temino"
ln -s ../../../../apps/Temino "$stage/data/deskbar/menu/Applications/Temino"

cp README.md LICENSE assets/fonts/JetBrainsMono-OFL.txt "$stage/documentation/packages/temino/"

sed -e "s/@VERSION@/$version/g" -e "s/@REVISION@/$rev/g" -e "s/@ARCH@/$arch/g" \
	haiku/PackageInfo.in > "$stage/.PackageInfo"

mkdir -p dist
rm -f "$out"
package create -C "$stage" "$out"
echo "$out"
