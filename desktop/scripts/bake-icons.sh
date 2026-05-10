#!/usr/bin/env bash
# Bake the desktop app's icon assets from `web/priv/apple-touch-icon.svg`.
#
# Tauri 2's bundler reads icons from `desktop/icons/` per the paths declared
# in `desktop/tauri.conf.json`. Rather than commit the rendered binary
# artefacts, we regenerate them from the source SVG using `rsvg-convert`
# (SVG→PNG) plus `cargo tauri icon` (PNG→multi-format: icon.icns / icon.ico /
# sized PNGs). Both tools come from the Nix dev shell — see `flake.nix`'s
# `devShells.default.buildInputs`.
#
# Idempotent: skips work when the cached output is up to date with the SVG.
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
