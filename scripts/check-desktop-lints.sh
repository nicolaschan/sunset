#!/usr/bin/env bash
# Catch silent drift between the workspace lint policy (root `Cargo.toml`'s
# `[workspace.lints]`) and the desktop crate's mirrored copy
# (`desktop/Cargo.toml`'s `[lints.*]`). The `desktop/` crate has its own
# Cargo.lock — see `Cargo.toml`'s `workspace.exclude = ["desktop"]` — so
# `[lints] workspace = true` doesn't work; we have to mirror by hand.
#
# Without this gate, adding a new lint in the workspace silently weakens
# the gate for the desktop crate and `cargo clippy -- -D warnings` won't
# notice. Fix is mechanical: paste the workspace `[lints]` body into
# `desktop/Cargo.toml`.
set -euo pipefail

cd "$(dirname "$0")/.."

# Extract the body of `[workspace.lints.<section>]` (root Cargo.toml) and
# `[lints.<section>]` (desktop Cargo.toml). Body = lines from the section
# header up to (but not including) the next `[…]` header, comments and
# trailing whitespace stripped, sorted for stable comparison.
#
# The exact section header (literal) comes in via `-v target=…`, sidestepping
# regex-quoting headaches between sed / awk / shell.
extract_section() {
  local file="$1" target="$2"
  awk -v target="$target" '
    $0 == target { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && NF { print }
  ' "$file" \
    | grep -v '^[[:space:]]*#' \
    | sed 's/[[:space:]]*$//' \
    | sort
}

mismatch=0
for section in rust clippy; do
  workspace_body="$(extract_section Cargo.toml "[workspace.lints.${section}]")"
  desktop_body="$(extract_section desktop/Cargo.toml "[lints.${section}]")"
  if [ "${workspace_body}" != "${desktop_body}" ]; then
    echo "Lint policy drift detected in [lints.${section}]:" >&2
    diff <(echo "${workspace_body}") <(echo "${desktop_body}") \
      --label "Cargo.toml [workspace.lints.${section}]" \
      --label "desktop/Cargo.toml [lints.${section}]" \
      --unified=99 || true
    mismatch=1
  fi
done

if [ "${mismatch}" -ne 0 ]; then
  echo "" >&2
  echo "Update desktop/Cargo.toml to match the workspace's [lints.*] sections," >&2
  echo "or update the workspace if the desktop's policy was the intended one." >&2
  exit 1
fi

echo "OK: desktop/Cargo.toml [lints.*] mirrors Cargo.toml [workspace.lints.*]."
