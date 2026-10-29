#!/bin/sh
# Builds Temino.app for Apple silicon and for Intel, as
# target/<target>/release/Temino.app, and packs each into
# target/release/Temino-<version>-macos-<arch>.dmg (also what the updater fetches);
# `macos/bundle.sh install` also copies this Mac's build to /Applications.
# Needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
set -e
cd "$(dirname "$0")/.."
version=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' macos/Info.plist)
# Signed ad hoc, macOS ties Screen Recording, Accessibility and the like to
# that one build's hash: every new build asks again though Settings shows
# Temino allowed. A certificate keeps the grants across builds:
# $TEMINO_SIGN, else this Mac's Developer ID, else its Apple Development
# identity, else ad hoc.
ids=$(security find-identity -v -p codesigning 2>/dev/null)
sign=${TEMINO_SIGN:-$(echo "$ids" | grep -oE '"Developer ID Application: [^"]*"' | head -1 | tr -d '"')}
sign=${sign:-$(echo "$ids" | grep -oE '"Apple Development: [^"]*"' | head -1 | tr -d '"')}
sign=${sign:--}
echo "signing as: $sign"
# Developer ID builds get the hardened runtime and a secure timestamp, and
# their DMGs are notarized and stapled so Gatekeeper opens them without
# "Open Anyway". Notarizing uses the notarytool keychain profile
# $TEMINO_NOTARY (default terminal-cam-notary); TEMINO_NOTARY=none or
# `bundle.sh install` skips it.
flags=
notary=
case "$sign" in "Developer ID Application:"*)
	flags="--options runtime --timestamp --entitlements macos/Temino.entitlements"
	[ "$1" = install ] || notary=${TEMINO_NOTARY:-terminal-cam-notary}
	[ "$notary" = none ] && notary= ;;
esac
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
	codesign --force $flags -s "$sign" "$app"
	mkdir -p target/release
	dmg=target/release/Temino-$version-macos-$arch.dmg
	stage=$(mktemp -d)/Temino
	mkdir -p "$stage"
	ditto "$app" "$stage/Temino.app"
	ln -s /Applications "$stage/Applications"
	hdiutil create -quiet -ov -volname Temino -srcfolder "$stage" -format UDZO "$dmg"
	if [ -n "$notary" ]; then
		codesign --force --timestamp -s "$sign" "$dmg"
		xcrun notarytool submit "$dmg" --keychain-profile "$notary" --wait
		xcrun stapler staple "$dmg"
	fi
	echo "built $app and $dmg"
done
if [ "$1" = install ]; then
	target=$([ "$(uname -m)" = arm64 ] && echo aarch64 || echo x86_64)-apple-darwin
	rm -rf /Applications/Temino.app
	cp -R target/$target/release/Temino.app /Applications/
	echo "installed /Applications/Temino.app"
fi
