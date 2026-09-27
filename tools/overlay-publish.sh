#!/bin/sh
# Push a StGit branch (default: overlay) together with its stack metadata.
#
# One atomic push: the branch may be force-updated (StGit rewrites it), but
# the stack ref must fast-forward. If someone else changed the published
# stack in the meantime, both refs are rejected and nothing changes on origin.
set -eu

b=${1:-overlay}
git rev-parse -q --verify "refs/stacks/$b" >/dev/null ||
    { echo "$b has no StGit stack (refs/stacks/$b)" >&2; exit 1; }
git push --atomic --force-with-lease="$b" origin "$b" "refs/stacks/$b"
