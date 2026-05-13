#!/usr/bin/env bash
# Workspace policy: clippy lints cannot be ignored — see CLAUDE.md.
# Reject `#[allow(clippy::...)]` and `#[expect(clippy::...)]` anywhere in
# our source tree. Lints from macro expansions inside dependencies are
# unaffected; this only scans files we own.
set -euo pipefail

cd "$(dirname "$0")/.."

# Match `#[allow(... clippy::X ...)]` and `#[expect(... clippy::X ...)]`
# (single-line). Multi-line attribute lists are not idiomatic in this
# tree; if that ever changes, extend the pattern accordingly.
pattern='#\[(allow|expect)\([^]]*clippy::'

# `desktop/` is a leaf Tauri crate outside the main workspace (it has its
# own Cargo.lock — see root `Cargo.toml`'s `workspace.exclude`). It still
# vendors the same lint policy in `desktop/Cargo.toml` and ships first-
# party Rust source we own, so it gets the same suppression ban.
sources=(crates desktop/src desktop/build.rs)

if hits=$(grep -RnE --include='*.rs' "$pattern" "${sources[@]}" 2>/dev/null); then
  echo "Source policy violation: clippy lint suppressions are not permitted." >&2
  echo "" >&2
  echo "$hits" >&2
  echo "" >&2
  echo "Fix the underlying clippy issue instead of suppressing it." >&2
  exit 1
fi

echo "OK: no #[allow(clippy::...)] / #[expect(clippy::...)] in ${sources[*]}."
