#!/bin/sh
# Make the local `overlay` branch and StGit stack an exact copy of a stack
# published on origin (branch + refs/stacks/<branch>):
#
#   overlay-adopt.sh overlay    recover the canonical stack, e.g. on a new
#                               machine or after it was changed elsewhere
#   overlay-adopt.sh <topic>    land a reviewed topic stack into overlay
#
# Refuses when that would drop local stack work that isn't in the incoming
# stack (pass -f to drop it anyway), or when the working tree is dirty.
set -eu

force=
[ "${1:-}" = -f ] && { force=1; shift; }
src=${1:?usage: overlay-adopt.sh [-f] <branch on origin>}
cd "$(git rev-parse --show-toplevel)"
git diff --quiet HEAD -- || { echo "working tree has uncommitted changes" >&2; exit 1; }

git fetch -q origin
branch=$(git rev-parse -q --verify "refs/remotes/origin/$src^{commit}") ||
    { echo "origin has no branch $src" >&2; exit 1; }
stack=$(git rev-parse -q --verify "refs/remote-stacks/origin/$src^{commit}") ||
    { echo "origin has no StGit stack for $src (refs/stacks/$src); publish it with overlay-publish.sh" >&2; exit 1; }

# The stack metadata must describe exactly that branch commit.
head=$(git show "$stack:stack.json" | sed -n 's/^ *"head": *"\([0-9a-f]*\)".*/\1/p')
[ "$head" = "$branch" ] ||
    { echo "origin/$src and its stack disagree (stack head $head); was only one of them pushed?" >&2; exit 1; }

# A topic must have been cloned from the current canonical stack.
if [ "$src" != overlay ] && canon=$(git rev-parse -q --verify refs/remote-stacks/origin/overlay); then
    git merge-base --is-ancestor "$canon" "$stack" ||
        { echo "$src was not cloned from the current overlay; sync its patches with 'stg sync -B $src <patch>...' instead" >&2; exit 1; }
fi

if local=$(git rev-parse -q --verify refs/stacks/overlay); then
    if [ -z "$force" ] && ! git merge-base --is-ancestor "$local" "$stack"; then
        echo "local overlay stack has changes that are not in $src; publish or drop them first (-f drops them)" >&2
        exit 1
    fi
    git update-ref refs/stacks-backup/overlay "$local"
fi

if git rev-parse -q --verify refs/heads/overlay >/dev/null; then
    git switch -q overlay
    git reset -q --hard "$branch"
else
    git switch -q --no-track -c overlay "$branch"
fi
git branch --unset-upstream 2>/dev/null || true
git update-ref refs/stacks/overlay "$stack"

echo "overlay is now $src (previous stack kept in refs/stacks-backup/overlay)"
stg series
