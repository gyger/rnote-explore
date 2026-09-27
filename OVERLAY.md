# Downstream overlay (StGit)

This checkout carries a set of local features (zoom window, dual-screen
presentation, Windows installer builds) on top of upstream
[flxzt/rnote](https://github.com/flxzt/rnote). The changes are kept as a
[StGit](https://stacked-git.github.io) patch stack so they can always be
replayed onto the latest upstream `main`.

This file and the helper scripts in `tools/` are the first patch of the
stack (`overlay-workflow`).

## Branches

| branch    | contents                                                     |
|-----------|--------------------------------------------------------------|
| `main`    | follows `upstream/main` exactly, never committed to          |
| `overlay` | upstream `main` + the StGit patch stack, the branch to build |

Remotes: `upstream` is `flxzt/rnote`, `origin` is the personal fork
(`gyger/rnote-explore`), where the overlay is published.

## Updating to the latest upstream

```bash
git switch overlay
git fetch upstream
git switch main && git merge --ff-only upstream/main && git switch overlay
stg rebase upstream/main      # pops all patches, moves base, re-pushes them
```

If a patch no longer applies, `stg rebase` stops at it with conflict markers:

```bash
# fix the files, then
git add <files>
stg refresh                   # record the resolution in the current patch
stg push --all                # continue with the remaining patches
```

Then verify the build (on Windows from an MSYS2 MinGW64 shell, see
`just prerequisites-win`):

```bash
just setup-dev                # once, creates _mesonbuild/
just check                    # fast: cargo check of ui and cli
just build && just test
```

Rnote-specific pitfalls:

- **meson source lists.** A new `.rs`, `.ui` or icon file must also be
  listed in the crate's `meson.build` (and `resources.gresource.xml` for
  resources). Otherwise ninja doesn't notice edits to it and skips the
  rebuild, or the resource is missing at runtime.
- **`Cargo.lock`.** Don't merge it by hand. Take upstream's version
  (`git checkout --ours Cargo.lock` during a StGit conflict), run
  `cargo update -w` so it picks up the patch's dependencies again, and
  `stg refresh` the result into the patch that added those dependencies.
- **Translations.** Patches don't touch `crates/rnote-ui/po/`. Upstream
  regenerates `rnote.pot`; new strings reach it from there.
- **Workflow drift.** `overlay-windows-build.yml` copies the build steps
  of upstream's `release-windows.yml`. When upstream changes those
  (toolchain, action versions), port the change into
  `windows-installer-ci`.

## Patches are intentions, not diffs

Each patch message says **why** the change exists and what it must keep
working, not only what lines it touches. When upstream restructures the code a
patch touches, reapplying the old hunks is often the wrong fix. Read the
patch description and reimplement the intent against the new upstream code,
then `stg refresh`. Update the description when the implementation changes.

When writing or editing a patch description, cover:

- **Purpose**: what user-visible behaviour or capability the patch provides.
- **Approach**: the key design decisions, the ones a reimplementation needs.
- **Constraints**: what must not change (stock rnote behaviour, the `.rnote`
  file format, which must stay readable by stock rnote).

`stg edit <patch>` edits a patch's message; `stg show <patch>` shows it.

## Working on the stack

```bash
stg series --description      # list patches with their subjects
stg new <name>                # start a new patch on top
stg refresh                   # fold working-tree changes into the top patch
stg refresh -p <name>         # ... or into a specific patch
stg goto <name>               # pop/push until <name> is on top
stg squash / stg pick / stg delete
```

Keep patches focused: bug fixes to an existing overlay feature can either
stay as their own patch (documents the history of the fix) or be squashed
into the feature patch once they are no longer interesting on their own.

Order matters: a patch may build on the ones below it (`dual-screen`
reuses `zoom-window`'s canvas pattern), never on the ones above.
Generic patches that don't depend on the features go low in the stack.

Upstream the patches that could be generally useful.
If upstream merges an equivalent change, `stg rebase` reports the patch as
empty; remove it with `stg delete`.

Patch names are stable identities: other stacks, `stg sync` and agents refer
to patches by name, so rename them only deliberately.

## Sharing the stack through GitHub

A plain `git push` / `git clone` only carries the `overlay` branch, i.e. the
patches as ordinary commits. StGit's own metadata (patch names, which patches
are applied, the stack's operation log) lives in a separate ref,
`refs/stacks/overlay`, a commit containing `stack.json`. GitHub stores it
like any other ref (it just isn't shown in the web UI), so the fork holds
both, and anyone can recover the exact stack from it:

```text
origin (gyger/rnote-explore)
├── refs/heads/overlay     ← the code: upstream main + patches as commits,
│                            usable by anyone who doesn't care about StGit
└── refs/stacks/overlay    ← StGit metadata for that branch
```

The per-patch refs `refs/patches/overlay/*` don't need sharing; StGit
recreates them from `refs/stacks/overlay`. The branch history is what
counts; the stack ref must always describe exactly that branch commit.

The git side lives in three scripts in `tools/`, because `.git/config`
isn't versioned:

| script | what it does |
|--------|--------------|
| `overlay-setup.sh` | one-time setup of a checkout: `upstream` remote, `main` tracking `upstream/main`, fetching stack refs, the `git overlay-adopt` / `git overlay-publish` / `git overlay-build` aliases, and restoring the `overlay` stack |
| `overlay-publish.sh [branch]` | push a branch and its stack ref in one atomic push (alias `git overlay-publish`) |
| `overlay-adopt.sh <branch>` | make local `overlay` + stack an exact copy of `<branch>` + its stack on origin (alias `git overlay-adopt`) |

### New checkout

```bash
git clone -b overlay https://github.com/gyger/rnote-explore.git rnote && cd rnote
sh tools/overlay-setup.sh           # ends by listing the patches
```

Stack refs are fetched into `refs/remote-stacks/origin/*`, **not** straight
into `refs/stacks/*`: fetching into `refs/stacks/*` would overwrite local
stack state (unpushed StGit work) on every `git fetch`. Local stacks change
only through `stg` or through `git overlay-adopt`, which refuses to drop
local stack work that the incoming stack doesn't contain, and keeps the
previous stack in `refs/stacks-backup/overlay`.

### Publish

```bash
git overlay-publish                 # = git push --atomic --force-with-lease=overlay origin overlay refs/stacks/overlay
```

The branch is rewritten by every rebase, so it needs a force push. The stack
ref doesn't: every StGit operation appends a commit to it, so a normal push
fast-forwards. That makes the stack ref the concurrency guard. If someone
else changed the published stack in the meantime, the push is rejected, and
thanks to `--atomic` the branch isn't updated either.

### Windows installer

GitHub builds the Windows installer for the commit tagged `build` (patch
`windows-installer-ci`, `.github/workflows/overlay-windows-build.yml`),
and only then. To build the published overlay, move the tag and push it:

```bash
git overlay-publish                 # the commit to build must be on origin
git overlay-build                   # = git tag -f build overlay && git push -f origin refs/tags/build
git overlay-build <commit>          # or build another commit
```

There is no versioning: each build replaces the previous one. The
installer is attached to a public prerelease on the `build` tag, always
under the same name, so this URL can be handed out once:

https://github.com/gyger/rnote-explore/releases/download/build/rnote-win-installer-overlay-x86_64.exe

The installed app reports e.g. `0.15.0+overlay-29ea24a` in its About
dialog; the release notes list the patches it contains.

Actions must be enabled for the fork. A tag pushed while Actions was off
doesn't build later; run `git overlay-build` again.

### Pick up changes made elsewhere

```bash
git overlay-adopt overlay           # fetch, then reset overlay + stack to origin's
```

If `stg` ever reports that the branch was modified outside StGit (e.g. after
a plain `git commit`), `stg repair` turns the extra commits into patches.

## Collaborating: one canonical stack, work on topic stacks

`overlay` is the one canonical stack. It changes only deliberately, one
change at a time. Nobody edits it concurrently. People and agents work on
their own StGit branches cloned from it and open a pull request into
`overlay` for review and CI:

```text
upstream/main
    └── overlay               canonical stack (branch + refs/stacks/overlay)
          ├── sam-smart-ink       topic stack: a new feature patch
          ├── sam-zoom-fix        topic stack: changes to some patches
          └── ai-rebase           an agent moving the stack to new upstream
```

Contributor:

```bash
git overlay-adopt overlay           # start from the current canonical stack
stg branch --clone sam-zoom-fix     # own branch + stack, same patches
# edit patches: stg goto / stg refresh / stg edit / stg new ...
git overlay-publish sam-zoom-fix
# open a PR on GitHub: sam-zoom-fix → overlay
```

Reviewers see the whole rewritten series in the PR. To review what changed
per patch, compare the two series:

```bash
git range-diff origin/overlay...origin/sam-zoom-fix
```

**Don't land the PR with GitHub's merge button.** None of its three modes
fits a patch stack:
- *merge* adds a merge commit, and `stg repair` would then unapply every
  patch below it;
- *squash* collapses the series into one commit;
- *rebase* replays the rewritten patches on top of the old ones.

The maintainer lands it by adopting the topic stack, which makes `overlay`
exactly the reviewed branch:

```bash
git overlay-adopt sam-zoom-fix      # checks it was cloned from the current overlay
git overlay-publish                 # overlay now contains the PR head, so GitHub should mark the PR merged
git push origin --delete sam-zoom-fix refs/stacks/sam-zoom-fix   # clean up
```

If `overlay` moved on since the topic was cloned, adopting is refused.
Bring the topic's changes over patch by patch instead, on the current
`overlay`:

```bash
git overlay-adopt overlay                              # current canonical stack
git fetch origin +refs/stacks/sam-zoom-fix:refs/stacks/sam-zoom-fix
git branch -f --no-track sam-zoom-fix origin/sam-zoom-fix   # local StGit view of the topic
stg sync -B sam-zoom-fix zoom-window                   # 3-way merge, same-named patch
git overlay-publish
```

`stg sync` merges each named patch against the base the patch sits on, so a
file that the patch itself adds always conflicts: take the topic's version
(`git checkout --theirs <file>`, `git add`, `stg refresh`). Only patch
content is synced, not descriptions; carry a changed description over with
`stg edit`. Name the patches explicitly. `stg sync --all` failed with
"Entry … would be overwritten by merge" (StGit 2.6.1) once it reached a
patch after an identical one.

### Upstream updates by an agent

The same shape works for an AI agent keeping the overlay current:

```bash
stg branch --clone ai-rebase
git fetch upstream && stg rebase upstream/main
```

`stg rebase` stops at the first patch that no longer applies, so the task is
always one patch at a time: *reapply `<patch>` to upstream `<commit>`,
preserving its documented purpose and constraints; the implementation may
change if upstream's architecture did.* The agent updates the patch
description if the approach changed, verifies the build, publishes
`ai-rebase` and opens a PR into `overlay`, which is landed as above.

### Plain patch files

`stg export -d patches/` writes the stack as plain patch files, e.g. for
review or for applying with `git am` elsewhere.
