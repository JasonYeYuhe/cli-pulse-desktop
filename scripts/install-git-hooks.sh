#!/usr/bin/env bash
# Install repo-local git hooks. Run once per clone:
#   scripts/install-git-hooks.sh
#
# Hooks live in `scripts/git-hooks/` and are symlinked into `.git/hooks/`.
# Skipping a single push: `git push --no-verify`.

set -euo pipefail
cd "$(dirname "$0")/.."

mkdir -p .git/hooks
for hook in scripts/git-hooks/*; do
    name=$(basename "$hook")
    target=".git/hooks/$name"
    rm -f "$target"
    ln -s "../../$hook" "$target"
    chmod +x "$hook"
    echo "installed: $target -> $hook"
done
