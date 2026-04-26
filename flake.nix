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
        fs = pkgs.lib.fileset;
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
        gleamLib = import ./nix/gleam { inherit pkgs; };
        webHexDeps =
          if builtins.pathExists ./web/manifest.toml
          then gleamLib.fetchHexDeps { manifest = ./web/manifest.toml; }
          else null;

        webNpmSrc = fs.toSource {
          root = ./.;
          fileset = fs.unions [
            ./web/package.json
            ./web/package-lock.json
          ];
        };

        webNpmDeps =
          if builtins.pathExists ./web/package-lock.json
          then pkgs.fetchNpmDeps {
            src = webNpmSrc + "/web";
            hash = "sha256-WkIJ90tJkCnbqafS1gTN2nnTzqSPZVF0TCZbmTFI9iU=";
          }
          else null;

        mkNodeModules = { name, src, npmDeps }: pkgs.stdenv.mkDerivation {
          inherit name src npmDeps;
          nativeBuildInputs = [ pkgs.nodejs pkgs.npmHooks.npmConfigHook ];
          npmRebuildFlags = [ "--ignore-scripts" ];
          dontBuild = true;
          installPhase = ''
            mkdir -p $out
            cp -r node_modules $out/node_modules
          '';
        };

        webNodeModules =
          if webNpmDeps != null
          then mkNodeModules {
            name = "sunset-web-node-modules";
            src = webNpmSrc + "/web";
            npmDeps = webNpmDeps;
          }
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
            # Lustre emits an absolute `/sunset_web.js` script src which only
            # works at site root. Rewrite to a relative path so the artefact
            # serves correctly under any GitHub Pages sub-path.
            ${pkgs.gnused}/bin/sed -i \
              's|src="/sunset_web\.js"|src="sunset_web.js"|' \
              $out/index.html
            # Pages serves /<repo>/index.html for root; tell Jekyll not to
            # touch the artefact (no underscores to start with, but defensive).
            touch $out/.nojekyll
            runHook postInstall
          '';
        };

        # End-to-end test runner. Builds node_modules + the prod dist, then
        # invokes Playwright with browsers from the Nix store. No network.
        webTestRunner = pkgs.writeShellScriptBin "sunset-web-test" ''
          set -euo pipefail

          export PATH="${pkgs.lib.makeBinPath [
            pkgs.nodejs
            pkgs.static-web-server
          ]}:$PATH"
          export PLAYWRIGHT_BROWSERS_PATH="${pkgs.playwright-driver.browsers}"
          export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=1
          export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
          export SUNSET_WEB_DIST="${webDist}"

          cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel)/web"
          # Link the Nix-built node_modules in place if it isn't there or is
          # pointing at something stale. Symlinks keep `npx playwright` happy.
          rm -f node_modules
          ln -sfn "${webNodeModules}/node_modules" node_modules

          exec node_modules/.bin/playwright test "$@"
        '';
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
            pkgs.static-web-server
          ];
          shellHook = ''
            ${if webHexDeps != null
              then gleamLib.devShellHook { gleamHexDeps = webHexDeps; }
              else ""}
            export PLAYWRIGHT_BROWSERS_PATH="${pkgs.playwright-driver.browsers}"
            export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=1
            export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
          '';
        };

        packages = pkgs.lib.optionalAttrs (webHexDeps != null) {
          web = webDist;
        } // pkgs.lib.optionalAttrs (webNpmDeps != null) {
          web-node-modules = webNodeModules;
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
        } // pkgs.lib.optionalAttrs (webHexDeps != null && webNpmDeps != null) {
          web-test = {
            type = "app";
            program = "${webTestRunner}/bin/sunset-web-test";
            meta.description = "Run the Playwright e2e suite against the prod build";
          };
        };
      });
}
