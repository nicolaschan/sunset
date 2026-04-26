{
  description = "sunset.chat";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };
  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
        gleamLib = import ./nix/gleam { inherit pkgs; };
        webHexDeps =
          if builtins.pathExists ./web/manifest.toml
          then gleamLib.fetchHexDeps { manifest = ./web/manifest.toml; }
          else null;

        webDist = gleamLib.buildGleamPackage {
          name = "sunset-web";
          src = ./web;
          manifest = ./web/manifest.toml;
          target = "javascript";
          lustre = true;
          buildPhase = ''
            runHook preBuild
            gleam run -m lustre/dev build sunset_web --minify
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p $out
            cp -r dist/* $out/
            runHook postInstall
          '';
        };
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-watch
            pkgs.cargo-nextest
            pkgs.gleam
            pkgs.erlang
            pkgs.rebar3
            pkgs.nodejs
            pkgs.bun
          ];
          shellHook =
            if webHexDeps != null
            then gleamLib.devShellHook { gleamHexDeps = webHexDeps; }
            else "";
        };

        packages = pkgs.lib.optionalAttrs (webHexDeps != null) {
          web = webDist;
        };

        apps = pkgs.lib.optionalAttrs (webHexDeps != null) {
          web-dev = {
            type = "app";
            program = "${pkgs.writeShellScriptBin "sunset-web-dev" ''
              export PATH="${pkgs.lib.makeBinPath [ pkgs.gleam pkgs.erlang pkgs.rebar3 pkgs.nodejs pkgs.bun ]}:$PATH"
              cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel 2>/dev/null || echo .)/web"
              ${gleamLib.devShellHook { gleamHexDeps = webHexDeps; }}
              exec ${pkgs.gleam}/bin/gleam run -m lustre/dev start
            ''}/bin/sunset-web-dev";
            meta.description = "Run the sunset.chat dev server with hot reload";
          };
        };
      });
}
