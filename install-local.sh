#!/usr/bin/env bash
# Install the locally-built AppDir over ~/.local/lib/screenpipe-ui.
#
# We install the AppDir contents directly rather than running the .AppImage:
# AppRun replaces GST_PLUGIN_SYSTEM_PATH with the bundle's own GStreamer
# plugins, which do not match CachyOS's registry and crash timeline playback.
# ~/.local/bin/screenpipe-ui launches usr/bin/screenpipe-app directly to skip
# AppRun entirely.
set -euo pipefail

cd "$(dirname "$0")"

APPDIR="apps/screenpipe-app-tauri/src-tauri/target/release/bundle/appimage/screenpipe - Development.AppDir"
DEST="$HOME/.local/lib/screenpipe-ui"

[[ -x "$APPDIR/usr/bin/screenpipe-app" ]] || {
	echo "error: no built app at $APPDIR" >&2
	echo "build it first: cd apps/screenpipe-app-tauri && bun tauri build --bundles appimage" >&2
	exit 1
}

if pgrep -x screenpipe-app >/dev/null; then
	echo "==> stopping running app"
	pkill -x screenpipe-app || true
	for _ in $(seq 20); do
		pgrep -x screenpipe-app >/dev/null || break
		sleep 0.5
	done
	pkill -9 -x screenpipe-app 2>/dev/null || true
fi

echo "==> installing to $DEST"
mkdir -p "$DEST"
# -L dereferences the AppDir's symlinks (.DirIcon, the .desktop) so the
# install does not depend on the build tree still existing.
rsync -a -L --delete "$APPDIR/" "$DEST/"

# Don't probe with --version: screenpipe-app has no such flag and treats it as a
# normal launch, which starts a second instance mid-install.
echo "==> installed $(stat -c '%y (%s bytes)' "$DEST/usr/bin/screenpipe-app")"

# Stopping the app above can leave the recorder down (and a port fight can push
# it past its systemd start limit into inactive). Put it back before the app
# returns, so we never hand back a machine that has quietly stopped recording.
echo "==> ensuring recorder is up"
systemctl --user reset-failed screenpipe.service 2>/dev/null || true
systemctl --user start screenpipe.service 2>/dev/null || true
for _ in $(seq 40); do
	[[ "$(systemctl --user is-active screenpipe.service)" == "active" ]] && break
	sleep 1
done
echo "==> recorder: $(systemctl --user is-active screenpipe.service)"

echo "start the app with: screenpipe-ui"
