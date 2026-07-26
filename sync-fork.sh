#!/usr/bin/env bash
# Rebase this fork's custom branch onto upstream screenpipe/main.
#
# The fork carries a small stack of Linux-only commits on top of upstream.
# Rebasing (not merging) keeps that stack readable as "our diff vs upstream",
# which is what makes conflicts small and reviewable each time.
#
#   ./sync-fork.sh            fetch + rebase, leave the result local
#   ./sync-fork.sh --push     also push the rebased branch to the fork remote
#   ./sync-fork.sh --check    report how far behind we are, change nothing
#
set -euo pipefail

cd "$(dirname "$0")"

UPSTREAM_URL_MATCH="screenpipe/screenpipe"
BRANCH="${SYNC_BRANCH:-custom/linux-timeline-stability}"

die() { echo "error: $*" >&2; exit 1; }

# Remotes here are named unconventionally (upstream is "origin", the fork is
# "fork"), so resolve them by URL instead of by name.
upstream_remote=""
fork_remote=""
while read -r name url _; do
	if [[ "$url" == *"$UPSTREAM_URL_MATCH"* ]]; then
		upstream_remote="$name"
	else
		fork_remote="$name"
	fi
done < <(git remote -v | awk '$3 == "(fetch)"')

[[ -n "$upstream_remote" ]] || die "no remote points at $UPSTREAM_URL_MATCH"
[[ -n "$fork_remote" ]] || die "no fork remote found"

echo "upstream: $upstream_remote   fork: $fork_remote   branch: $BRANCH"

# Upstream has hundreds of agent/* branches; fetching only main keeps this
# from pulling tens of MB of refs we never look at.
echo "==> fetching $upstream_remote/main"
git fetch --no-tags "$upstream_remote" main

base=$(git merge-base "$BRANCH" "$upstream_remote/main")
behind=$(git rev-list --count "$base..$upstream_remote/main")
ours=$(git rev-list --count "$base..$BRANCH")
echo "==> $behind new upstream commit(s); $ours local commit(s) on top"

if [[ "${1:-}" == "--check" ]]; then
	[[ "$behind" -eq 0 ]] && echo "up to date." || \
		git log --oneline "$base..$upstream_remote/main" | head -20
	exit 0
fi

if [[ "$behind" -eq 0 ]]; then
	echo "already up to date with upstream."
else
	echo "==> rebasing $BRANCH onto $upstream_remote/main"
	# --autostash: the Tauri build regenerates src-tauri/gen/schemas/*, so the
	# worktree is almost never clean at sync time. Those are build artifacts;
	# stashing and reapplying them is correct, not a workaround.
	if ! git rebase --autostash "$upstream_remote/main" "$BRANCH"; then
		cat <<-'EOF'

		Rebase stopped on a conflict. Resolve it, then:
		    git add <files> && git rebase --continue
		To bail out entirely:
		    git rebase --abort
		EOF
		exit 1
	fi
fi

if [[ "${1:-}" == "--push" ]]; then
	echo "==> pushing to $fork_remote"
	# --force-with-lease, never plain --force: a rebase rewrites history, but
	# this still refuses if someone else moved the fork branch.
	git push --force-with-lease "$fork_remote" "$BRANCH"
fi

cat <<-EOF

Done. $BRANCH is now upstream/main + $ours local commit(s).

Rebuild and reinstall:
    cd apps/screenpipe-app-tauri
    bun install && bun tauri build --bundles appimage
    ./install-local.sh
EOF
