#!/bin/sh
# Installs Temino for the current user: the binary, the app-menu entry and
# its icon. Run it from the unpacked release folder or from a source checkout.
#   ./install.sh              into ~/.local
#   ./install.sh --system     into /usr/local (run with sudo)
#   ./install.sh --uninstall  [--system] removes what it installed
set -e

prefix="$HOME/.local"
uninstall=
for arg; do
	case $arg in
	--system) prefix=/usr/local ;;
	--uninstall) uninstall=1 ;;
	*) echo "usage: $0 [--system] [--uninstall]" >&2; exit 2 ;;
	esac
done

here=$(cd "$(dirname "$0")" && pwd)
bin="$prefix/bin/temino"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"
# GNOME's hicolor theme looks in these sizes only; nothing above 512.
sizes="512 256 128 64 48 32"

refresh() {
	command -v gtk-update-icon-cache >/dev/null &&
		gtk-update-icon-cache -f -t -q "$icons" 2>/dev/null || true
	command -v update-desktop-database >/dev/null &&
		update-desktop-database -q "$apps" 2>/dev/null || true
}

if [ -n "$uninstall" ]; then
	rm -f "$bin" "$apps/temino.desktop"
	for s in $sizes 1024; do rm -f "$icons/${s}x$s/apps/temino.png"; done
	refresh
	echo "Removed Temino from $prefix"
	exit 0
fi

find_file() {
	for f; do [ -f "$here/$f" ] && { echo "$here/$f"; return; }; done
	echo "install.sh: can't find $1 next to this script" >&2
	exit 1
}
src_bin=$(find_file temino ../target/release/temino)
src_icon=$(find_file icon.png ../docs/icon.png)
src_desktop=$(find_file temino.desktop)

mkdir -p "$prefix/bin" "$apps"
# Copy then rename, so a running Temino keeps its old binary.
cp "$src_bin" "$bin.new" && chmod 755 "$bin.new" && mv -f "$bin.new" "$bin"

# Exec gets the full path: the desktop session's PATH often lacks ~/.local/bin.
sed "s|^Exec=.*|Exec=$bin|" "$src_desktop" >"$apps/temino.desktop"

resize=
if command -v magick >/dev/null; then resize=magick
elif command -v convert >/dev/null; then resize=convert
fi
rm -f "$icons/1024x1024/apps/temino.png" # older hand installs; never looked up
for s in $sizes; do
	mkdir -p "$icons/${s}x$s/apps"
	if [ -n "$resize" ]; then
		$resize "$src_icon" -resize "${s}x$s" "$icons/${s}x$s/apps/temino.png"
	elif [ "$s" = 512 ]; then
		# No resizer: the full-size icon in the 512 folder still gets scaled.
		cp "$src_icon" "$icons/512x512/apps/temino.png"
	fi
done
refresh

echo "Installed Temino to $bin; it's in the app menu as Temino."
case ":$PATH:" in *":$prefix/bin:"*) ;; *) echo "Add $prefix/bin to PATH to run 'temino' from a shell." ;; esac
