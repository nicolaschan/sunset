#!/usr/bin/env bash
# Regenerate the desktop app's icon assets from `web/priv/apple-touch-icon.svg`.
#
# The rendered icons (`desktop/icons/{32x32,128x128,128x128@2x,icon.png}`,
# `icon.icns`, `icon.ico`) **are committed** to the repo. We don't bake them
# at every build — Tauri's compile-time embedding wants them on disk before
# `cargo build` runs, and committing them keeps `cargo build -p sunset-desktop`
# working from a clean clone without first hitting `librsvg` / `cargo-tauri`.
# Run this script after editing the source SVG; CI does not re-run it.
#
# Tooling comes from the Nix dev shell — `rsvg-convert` (SVG→PNG, from
# `pkgs.librsvg`) plus `cargo tauri icon` (PNG→multi-format, from
# `pkgs.cargo-tauri`). Run inside `nix develop` or via `nix develop --command`.
#
# Idempotent: skips work when the stamp is newer than the source SVG and
# every required output is present.
set -euo pipefail

# Resolve the workspace root from this script's location so it works whether
# invoked from the workspace root, the desktop/ directory, or elsewhere.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE_ROOT="$(cd "${DESKTOP_DIR}/.." && pwd)"

SOURCE_SVG="${WORKSPACE_ROOT}/web/priv/apple-touch-icon.svg"
ICONS_DIR="${DESKTOP_DIR}/icons"
STAMP_FILE="${ICONS_DIR}/.bake-stamp"

if [ ! -f "${SOURCE_SVG}" ]; then
  echo "bake-icons: source SVG not found at ${SOURCE_SVG}" >&2
  exit 1
fi

# Skip if the stamp is newer than the source SVG and the expected outputs exist.
required_outputs=(
  "${ICONS_DIR}/32x32.png"
  "${ICONS_DIR}/128x128.png"
  "${ICONS_DIR}/128x128@2x.png"
  "${ICONS_DIR}/icon.png"
  "${ICONS_DIR}/icon.icns"
  "${ICONS_DIR}/icon.ico"
)
if [ -f "${STAMP_FILE}" ] && [ "${STAMP_FILE}" -nt "${SOURCE_SVG}" ]; then
  all_present=1
  for out in "${required_outputs[@]}"; do
    [ -f "${out}" ] || { all_present=0; break; }
  done
  if [ "${all_present}" -eq 1 ]; then
    exit 0
  fi
fi

mkdir -p "${ICONS_DIR}"

# Render a 1024x1024 master PNG from the SVG. `cargo tauri icon` consumes this
# and emits the rest of the assets (sized PNGs + icns + ico).
rsvg-convert -w 1024 -h 1024 "${SOURCE_SVG}" -o "${ICONS_DIR}/icon.png"

# `cargo tauri icon` writes outputs to `<config_dir>/icons/`. It autodetects
# the config dir from a `tauri.conf.json` in the current dir. Run from
# `desktop/` so it lands in `desktop/icons/`.
(
  cd "${DESKTOP_DIR}"
  cargo tauri icon icons/icon.png --output icons >/dev/null
)

# `cargo tauri icon` also emits iOS / Android / Windows tile assets we don't
# need for the desktop bundle. Strip them so the source tree stays small and
# the icon directory matches the file list in `desktop/tauri.conf.json`.
(
  cd "${ICONS_DIR}"
  rm -rf android ios vendor
  # macOS / Windows app store tiles, only relevant if we ship Universal Windows
  # / iPadOS bundles (we don't).
  rm -f Square*.png StoreLogo.png 64x64.png
)

touch "${STAMP_FILE}"
echo "bake-icons: regenerated icons in ${ICONS_DIR}"
