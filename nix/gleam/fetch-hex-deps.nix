# fetchHexDeps: parse a Gleam manifest.toml and produce a hex cache derivation
#
# The output is a store path with Gleam's expected hex cache layout. Gleam
# stores tarballs under TWO names in its cache: the human-friendly
# `<name>-<version>.tar` and the content-addressed `<UPPERCASE_CHECKSUM>.tar`.
# Newer gleam versions (1.15+) look up by checksum first, so we populate both.
#
#   $out/gleam/hex/hexpm/packages/<name>-<version>.tar
#   $out/gleam/hex/hexpm/packages/<UPPERCASE_CHECKSUM>.tar
#
# Usage:
#   fetchHexDeps { manifest = ./manifest.toml; }
#
{ pkgs }:

{ manifest }:

let
  parsed = builtins.fromTOML (builtins.readFile manifest);

  # Each entry carries the raw checksum (used as filename) alongside the fetched tarball
  pkgsWithDrv = map (pkg: {
    name = pkg.name;
    version = pkg.version;
    checksum = pkg.outer_checksum;
    drv = pkgs.fetchurl {
      url = "https://repo.hex.pm/tarballs/${pkg.name}-${pkg.version}.tar";
      sha256 = pkgs.lib.toLower pkg.outer_checksum;
      name = "${pkg.name}-${pkg.version}.tar";
    };
  }) parsed.packages;

  hexDeps = map (p: p.drv) pkgsWithDrv;

in
pkgs.runCommand "gleam-hex-deps" {} ''
  mkdir -p $out/gleam/hex/hexpm/packages
  ${pkgs.lib.concatMapStringsSep "\n" (p: ''
    cp ${p.drv} $out/gleam/hex/hexpm/packages/${p.name}-${p.version}.tar
    ln -sfn ${p.name}-${p.version}.tar $out/gleam/hex/hexpm/packages/${p.checksum}.tar
  '') pkgsWithDrv}
'' // {
  # Expose the individual tarballs for consumers that need them (e.g. devShellHook)
  inherit hexDeps;
}
