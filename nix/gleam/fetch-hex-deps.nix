# fetchHexDeps: parse a Gleam manifest.toml and produce a hex cache derivation
#
# The output is a store path with Gleam's expected hex cache layout:
#   $out/gleam/hex/hexpm/packages/<name>-<version>.tar
#
# Usage:
#   fetchHexDeps { manifest = ./manifest.toml; }
#
{ pkgs }:

{ manifest }:

let
  parsed = builtins.fromTOML (builtins.readFile manifest);

  hexDeps = map (pkg: pkgs.fetchurl {
    url = "https://repo.hex.pm/tarballs/${pkg.name}-${pkg.version}.tar";
    sha256 = pkgs.lib.toLower pkg.outer_checksum;
    name = "${pkg.name}-${pkg.version}.tar";
  }) parsed.packages;

in
pkgs.runCommand "gleam-hex-deps" {} ''
  mkdir -p $out/gleam/hex/hexpm/packages
  ${pkgs.lib.concatMapStringsSep "\n" (drv:
    "cp ${drv} $out/gleam/hex/hexpm/packages/${drv.name}"
  ) hexDeps}
'' // {
  # Expose the individual tarballs for consumers that need them (e.g. devShellHook)
  inherit hexDeps;
}
