#!/bin/sh
# Builds Temino.app for Apple silicon and for Intel, as
# target/<target>/release/Temino.app, and zips each into
# target/release/Temino-<version>-macos-<arch>.zip;
# `macos/bundle.sh install` also copies this Mac's build to /Applications.
# Needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
set -e
cd "$(dirname "$0")/.."
version=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' macos/Info.plist)
icons=$(mktemp -d)/Temino.iconset
mkdir -p "$icons"
for size in 16 32 64 128 256 512; do
	sips -z $size $size docs/icon.png --out "$icons/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128 256; do
	cp "$icons/icon_$((size * 2))x$((size * 2)).png" "$icons/icon_${size}x${size}@2x.png"
done
for arch in arm64 x86_64; do
	target=$([ $arch = arm64 ] && echo aarch64 || echo x86_64)-apple-darwin
	cargo build --release --target $target
	app=target/$target/release/Temino.app
	rm -rf "$app"
	mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
	cp target/$target/release/temino "$app/Contents/MacOS/temino"
	cp macos/Info.plist "$app/Contents/Info.plist"
	iconutil -c icns "$icons" -o "$app/Contents/Resources/Temino.icns"
	codesign --force -s - "$app"
	zip=target/release/Temino-$version-macos-$arch.zip
	mkdir -p target/release
	rm -f "$zip"
	ditto -c -k --keepParent "$app" "$zip"
	echo "built $app and $zip"
done
if [ "$1" = install ]; then
	target=$([ "$(uname -m)" = arm64 ] && echo aarch64 || echo x86_64)-apple-darwin
	rm -rf /Applications/Temino.app
	cp -R target/$target/release/Temino.app /Applications/
	echo "installed /Applications/Temino.app"
fi
