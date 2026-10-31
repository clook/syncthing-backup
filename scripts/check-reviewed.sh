#!/usr/bin/env bash
# Checks that the code in <commit> (default HEAD) is covered by a review record in docs/reviews/.
# A record covers the code if its `Reviewed-Commit:` is an ancestor of <commit> and no code path
# changed between them (later commits may only touch docs).
set -euo pipefail
cd "$(dirname "$0")/.."
commit=${1:-HEAD}
code_paths=(src Cargo.toml Cargo.lock Dockerfile build.rs)

latest=$(git ls-tree --name-only "$commit" docs/reviews/ 2>/dev/null | grep -E '/[0-9]{4}-.*\.md$' | sort | tail -n1 || true)
if [[ -z $latest ]]; then
  echo "check-reviewed: no review record in docs/reviews/ at $commit"; exit 1
fi
reviewed=$(git show "$commit:$latest" | sed -nE 's/^Reviewed-Commit: *([0-9a-f]{7,40}).*/\1/p' | head -n1)
if [[ -z $reviewed ]]; then
  echo "check-reviewed: $latest has no 'Reviewed-Commit: <sha>' line"; exit 1
fi
if ! git merge-base --is-ancestor "$reviewed" "$commit"; then
  echo "check-reviewed: $reviewed (from $latest) is not an ancestor of $commit"; exit 1
fi
if ! git diff --quiet "$reviewed" "$commit" -- "${code_paths[@]}"; then
  echo "check-reviewed: code changed since the last review ($latest, $reviewed):"
  git diff --stat "$reviewed" "$commit" -- "${code_paths[@]}"
  exit 1
fi
echo "check-reviewed: $commit is covered by $latest"
