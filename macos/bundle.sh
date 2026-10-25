#!/bin/sh
# Builds Temino.app from a release build; `macos/bundle.sh install` also
# copies it to /Applications.
set -e
cd "$(dirname "$0")/.."
cargo build --release
app=target/release/Temino.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/temino "$app/Contents/MacOS/temino"
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
