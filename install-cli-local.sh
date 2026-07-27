#!/usr/bin/env bash
# Install the locally-built `screenpipe` CLI over the one on PATH.
#
# Chronicle does not install ScreenPipe: extras/screenpipe-collector/init.py
# resolves it with `shutil.which("screenpipe")` and writes a systemd unit around
# whatever it finds. On this machine that is the npm @screenpipe/cli-linux-x64
# build, so changes to this fork never reach the recorder until the symlink is
# repointed — which is what this script does.
#
# The binary is copied out of target/ first: a `cargo clean` must not be able to
# take the recorder down. Same reasoning as install-local.sh for the app.
#
# The previous symlink target is saved next to the install so rollback is one
# command. Run with --revert to restore it.
set -euo pipefail

cd "$(dirname "$0")"

BIN="target/release/screenpipe"
DEST="$HOME/.local/lib/screenpipe-cli-local"
LINK="$HOME/.local/bin/screenpipe"
BACKUP="$DEST/previous-target"
UNIT="screenpipe.service"

revert() {
	[[ -f "$BACKUP" ]] || {
		echo "error: no saved target at $BACKUP" >&2
		exit 1
	}
	local prev
	prev="$(cat "$BACKUP")"
	[[ -e "$prev" ]] || {
		echo "error: saved target no longer exists: $prev" >&2
		exit 1
	}
	ln -sfn "$prev" "$LINK"
	echo "==> reverted $LINK -> $prev"
	systemctl --user restart "$UNIT"
	echo "==> restarted $UNIT ($("$LINK" --version))"
	exit 0
}

[[ "${1:-}" == "--revert" ]] && revert

[[ -x "$BIN" ]] || {
	echo "error: no built binary at $BIN" >&2
	echo "build it first: cargo build --release -p screenpipe-engine --bin screenpipe" >&2
	exit 1
}

mkdir -p "$DEST"

# Save the current target once. Re-running must not overwrite the record of the
# original npm build with our own copy.
if [[ ! -f "$BACKUP" ]] && [[ -L "$LINK" ]]; then
	readlink -f "$LINK" >"$BACKUP"
	echo "==> saved previous target: $(cat "$BACKUP")"
fi

# Stop the recorder before replacing the file it is executing.
was_active=0
if systemctl --user is-active --quiet "$UNIT"; then
	was_active=1
	echo "==> stopping $UNIT"
	systemctl --user stop "$UNIT"
fi

install -m 0755 "$BIN" "$DEST/screenpipe"
ln -sfn "$DEST/screenpipe" "$LINK"
echo "==> installed $DEST/screenpipe ($("$LINK" --version))"

if ((was_active)); then
	systemctl --user start "$UNIT"
	echo "==> started $UNIT"
fi
