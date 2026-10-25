#!/bin/sh
# Builds a universal (arm64 + x86_64) Temino.app from release builds;
# `macos/bundle.sh install` also copies it to /Applications.
# Needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
set -e
cd "$(dirname "$0")/.."
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	cargo build --release --target $target
done
app=target/release/Temino.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/temino" \
	target/aarch64-apple-darwin/release/temino target/x86_64-apple-darwin/release/temino
cp macos/Info.plist "$app/Contents/Info.plist"
icons=$(mktemp -d)/Temino.iconset
mkdir -p "$icons"
for size in 16 32 64 128 256 512; do
	sips -z $size $size docs/icon.png --out "$icons/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128 256; do
	cp "$icons/icon_$((size * 2))x$((size * 2)).png" "$icons/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$icons" -o "$app/Contents/Resources/Temino.icns"
codesign --force -s - "$app"
echo "built $app"
if [ "$1" = install ]; then
	rm -rf /Applications/Temino.app
	cp -R "$app" /Applications/
	echo "installed /Applications/Temino.app"
fi
