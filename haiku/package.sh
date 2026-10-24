#!/bin/bash
# Builds Renga and wraps it in an .hpkg. Run ON Haiku:
#   haiku/package.sh                     -> dist/renga-<version>-<rev>-<arch>.hpkg
#   RENGA_PKG_REV=2 haiku/package.sh     bump the package revision
# Install by copying the .hpkg into ~/config/packages (activates at once).
# Renga needs a Vulkan driver: libvulkan.so.1 on the library path (on the
# UTM VM, the Venus stack in ~/config/non-packaged/lib).
set -eo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rev=${RENGA_PKG_REV:-1}
arch=$(getarch)
out="dist/renga-$version-$rev-$arch.hpkg"

cargo build --release

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/apps" "$stage/bin" "$stage/data/deskbar/menu/Applications" \
	"$stage/documentation/packages/renga"

app="$stage/apps/Renga"
install -m 755 target/release/renga "$app"
rc -o "$stage/Renga.rsrc" haiku/Renga.rdef
xres -o "$app" "$stage/Renga.rsrc"
rm "$stage/Renga.rsrc"
mimeset -f "$app"
ln -s ../apps/Renga "$stage/bin/renga"
ln -s ../../../../apps/Renga "$stage/data/deskbar/menu/Applications/Renga"

cp README.md LICENSE assets/fonts/JetBrainsMono-OFL.txt "$stage/documentation/packages/renga/"

sed -e "s/@VERSION@/$version/g" -e "s/@REVISION@/$rev/g" -e "s/@ARCH@/$arch/g" \
	haiku/PackageInfo.in > "$stage/.PackageInfo"

mkdir -p dist
rm -f "$out"
package create -C "$stage" "$out"
echo "$out"
