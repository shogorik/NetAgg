#!/usr/bin/env bash
# Puts apt-repo/ (made by `scripts/release.sh --linux-from`) on the gh-pages branch as apt/, the
# folder GitHub Pages serves at https://shogorik.github.io/NetAgg/apt/, and commits it locally:
#
#   scripts/publish-apt.sh
#
# It never pushes. Publishing is `git push origin gh-pages`, done by hand, once the repository has
# been checked (and, the first time, GitHub Pages turned on for the gh-pages branch).
set -euo pipefail

cd "$(dirname "$0")/.."
SRC="apt-repo"
die() { echo "publish-apt: $*" >&2; exit 1; }

for f in InRelease Release Release.gpg Packages Packages.gz netagg-archive-keyring.gpg; do
  [[ -f "$SRC/$f" ]] || die "missing $SRC/$f: run scripts/release.sh --linux-from first"
done
ls "$SRC"/netagg-server_*.deb > /dev/null 2>&1 || die "no netagg-server package in $SRC"

WORK="$(mktemp -d)"
trap 'git worktree remove --force "$WORK" 2>/dev/null || rm -rf "$WORK"' EXIT
if git show-ref --verify --quiet refs/heads/gh-pages; then
  git worktree add --quiet "$WORK" gh-pages
elif git show-ref --verify --quiet refs/remotes/origin/gh-pages; then
  git worktree add --quiet -b gh-pages "$WORK" origin/gh-pages
else
  # First time: a branch with no history, holding only the published files.
  git worktree add --quiet --detach "$WORK"
  git -C "$WORK" checkout --quiet --orphan gh-pages
  git -C "$WORK" rm -rf --quiet .
  # Serve files as they are (no Jekyll processing).
  touch "$WORK/.nojekyll"
fi

# Byte for byte: a line-ending conversion (core.autocrlf) would break the signatures.
printf '* -text\n' > "$WORK/.gitattributes"
# Only the newest package: apt installs the newest anyway, and Pages has a size limit.
rm -rf "$WORK/apt"
mkdir -p "$WORK/apt"
cp "$SRC"/* "$WORK/apt/"
VERSION="$(ls "$SRC"/netagg-server_*.deb | sed -E 's/.*netagg-server_([^_]+)_.*/\1/')"
git -C "$WORK" add -A
if git -C "$WORK" diff --cached --quiet; then
  echo "publish-apt: gh-pages already has this repository"
else
  git -C "$WORK" commit --quiet -m "apt repository: netagg-server $VERSION"
  echo "publish-apt: committed to gh-pages (not pushed):"
  git -C "$WORK" log --oneline -1
  git -C "$WORK" show --stat --oneline HEAD | tail -n +2
fi
echo "To publish: git push origin gh-pages"
