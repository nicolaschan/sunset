# Gleam setup hooks, analogous to npmHooks in nixpkgs.
#
# gleamConfigHook: populates the hex cache from a fetchHexDeps derivation,
# optionally configures Lustre to use system bun, and runs `gleam deps download`.
# Registered as a postPatch hook.
#
# Usage: add to nativeBuildInputs and set gleamHexDeps on the derivation:
#
#   nativeBuildInputs = [ gleamHooks.gleamConfigHook ];
#   gleamHexDeps = fetchHexDeps { manifest = ./manifest.toml; };
#   gleamUseSystemBun = true;  # optional, for Lustre projects
#
{ pkgs }:

{
  gleamConfigHook = pkgs.makeSetupHook {
    name = "gleam-config-hook";
    substitutions = {
      gleam = "${pkgs.gleam}";
    };
  } ./gleam-config-hook.sh;
}
