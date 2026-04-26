# buildGleamPackage: high-level wrapper around stdenv.mkDerivation for Gleam projects.
#
# Analogous to buildNpmPackage in nixpkgs. Automatically:
#   - Adds gleam, erlang, rebar3 to nativeBuildInputs
#   - Adds nodejs (and optionally bun) for JavaScript-target projects
#   - Wires in gleamConfigHook to populate the hex cache
#   - Creates gleamHexDeps from manifest if not provided directly
#   - Sets dontStrip and dontFixup (JS/BEAM output shouldn't be stripped)
#
# Usage:
#   buildGleamPackage {
#     name = "my-app";
#     src = ./.;
#     manifest = ./manifest.toml;
#     target = "javascript";  # or "erlang" (default)
#     lustre = true;          # adds bun, configures system bun for Lustre
#     buildPhase = "gleam run -m lustre/dev build my_app --minify";
#     installPhase = "cp -r dist $out";
#   }
#
{ pkgs, fetchHexDeps, gleamHooks }:

{
  name ? "gleam-package",
  src,
  manifest,
  target ? "erlang",
  lustre ? false,
  gleamHexDeps ? null,
  nativeBuildInputs ? [],
  ...
}@args:

let
  isJsTarget = target == "javascript";
  resolvedHexDeps = if gleamHexDeps != null
    then gleamHexDeps
    else fetchHexDeps { inherit manifest; };

  # Base build inputs for any Gleam project
  gleamBuildInputs = with pkgs; [ gleam erlang rebar3 ]
    ++ pkgs.lib.optionals isJsTarget [ nodejs ]
    ++ pkgs.lib.optionals lustre [ bun ];

  # Remove our custom args before passing to mkDerivation
  cleanArgs = builtins.removeAttrs args [
    "manifest" "target" "lustre" "gleamHexDeps"
  ];

in
pkgs.stdenv.mkDerivation (cleanArgs // {
  inherit name src;

  nativeBuildInputs = gleamBuildInputs
    ++ [ gleamHooks.gleamConfigHook ]
    ++ nativeBuildInputs;

  gleamHexDeps = resolvedHexDeps;
  gleamUseSystemBun = lustre;

  dontStrip = args.dontStrip or true;
  dontFixup = args.dontFixup or true;
})
