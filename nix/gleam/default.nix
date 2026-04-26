# Gleam build support for Nix.
#
# Provides fetchHexDeps, gleamHooks, buildGleamPackage, and devShellHook —
# a complete toolkit for building Gleam projects with Nix, analogous to
# nixpkgs' fetchNpmDeps / npmHooks / buildNpmPackage.
#
# Usage:
#   let
#     gleamLib = import ./nix/gleam { inherit pkgs; };
#   in {
#     packages.default = gleamLib.buildGleamPackage { ... };
#     devShells.default = pkgs.mkShell {
#       shellHook = gleamLib.devShellHook { ... };
#     };
#   }
#
{ pkgs }:

let
  fetchHexDeps = import ./fetch-hex-deps.nix { inherit pkgs; };
  gleamHooks = import ./hooks { inherit pkgs; };
  buildGleamPackage = import ./build-gleam-package.nix { inherit pkgs fetchHexDeps gleamHooks; };

  # devShellHook: generates a shell script that populates the hex cache
  # using symlinks (fast, no copies) and runs `gleam deps download`.
  # For use in devShells.default.shellHook.
  #
  # Usage:
  #   devShells.default = pkgs.mkShell {
  #     shellHook = gleamLib.devShellHook {
  #       gleamHexDeps = gleamLib.fetchHexDeps { manifest = ./manifest.toml; };
  #     };
  #   };
  devShellHook = { gleamHexDeps }: ''
    # Populate hex cache from Nix store (symlinks for speed)
    mkdir -p "$HOME/.cache/gleam/hex/hexpm/packages"
    ${pkgs.lib.concatMapStringsSep "\n" (drv:
      "ln -sfn ${drv} \"$HOME/.cache/gleam/hex/hexpm/packages/${drv.name}\""
    ) gleamHexDeps.hexDeps}

    # Download gleam deps (reads from pre-populated hex cache, no network)
    ${pkgs.gleam}/bin/gleam deps download 2>/dev/null || true
  '';

in
{
  inherit fetchHexDeps gleamHooks buildGleamPackage devShellHook;
}
