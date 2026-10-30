#!/usr/bin/env bash
# Mechanical review gate: formatting, lints, tests and the safety invariants from CLAUDE.md.
# Runs every check and fails at the end if any of them failed.
set -uo pipefail
cd "$(dirname "$0")/.."

failed=()
step() {
  local name=$1; shift
  echo "==> $name"
  if "$@"; then echo "    ok"; else echo "    FAILED"; failed+=("$name"); fi
}

# Production code only: everything before the first `#[cfg(test)]` of each file.
prod_code() {
  for f in src/*.rs; do
    awk -v f="$f" '/^#\[cfg\(test\)\]/{exit} {print f":"FNR": "$0}' "$f"
  done
}

# Fails (and prints the offending lines) if the pattern appears in production code outside allowed files.
forbid() {
  local pattern=$1 allowed=${2:-}
  local hits
  hits=$(prod_code | grep -E "$pattern" | grep -vE '^\S+:[0-9]+: *//' || true)
  if [[ -n $allowed ]]; then hits=$(grep -vE "^($allowed):" <<<"$hits" || true); fi
  if [[ -n $hits ]]; then echo "$hits"; return 1; fi
}

step "cargo fmt --check" cargo fmt --all -- --check
step "cargo clippy (deny warnings)" cargo clippy --all-targets --quiet -- -D warnings
step "cargo test" cargo test --quiet
step "invariant: no unwrap/expect in production code" forbid '\.unwrap\(\)|\.expect\('
step "invariant: only mover.rs deletes, renames or copies files" \
  forbid 'remove_file|remove_dir|fs::rename|fs::copy|hard_link|io::copy' 'src/mover.rs'
step "invariant: no Redis dependency" bash -c '! grep -q "^redis" Cargo.toml'

if command -v cargo-audit >/dev/null; then
  step "cargo audit" cargo audit --quiet
else
  echo "==> cargo audit: skipped (cargo install cargo-audit to enable)"
fi

if ((${#failed[@]})); then
  printf '\nreview.sh: %d check(s) failed:\n' "${#failed[@]}"; printf '  - %s\n' "${failed[@]}"
  exit 1
fi
echo; echo "review.sh: all checks passed"
