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

if hits=$(grep -RnE --include='*.rs' "$pattern" crates 2>/dev/null); then
  echo "Workspace policy violation: clippy lint suppressions are not permitted." >&2
  echo "" >&2
  echo "$hits" >&2
  echo "" >&2
  echo "Fix the underlying clippy issue instead of suppressing it." >&2
  exit 1
fi

echo "OK: no #[allow(clippy::...)] / #[expect(clippy::...)] in crates/."
