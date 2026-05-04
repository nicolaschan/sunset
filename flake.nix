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
            hash = "sha256-ZjoBtc99+jQS3xuYdrcrBN4uVM1ow8HZO9d60rIJ2e8=";
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

        # Helper to build sunset-web-wasm with an optional feature set.
        mkSunsetWebWasmPkg = { features ? [] }: pkgs.rustPlatform.buildRustPackage {
          pname = "sunset-web-wasm";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false;
          nativeBuildInputs = [ pkgs.wasm-bindgen-cli pkgs.lld ];
          cargo = rustToolchain;
          rustc = rustToolchain;
          buildPhase = ''
            runHook preBuild
            cargo build \
              -j $NIX_BUILD_CORES \
              --offline \
              --release \
              --target wasm32-unknown-unknown \
              -p sunset-web-wasm \
              --lib \
              ${pkgs.lib.optionalString (features != []) ("--features " + builtins.concatStringsSep "," features)}
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            wasm-bindgen \
              --target web \
              --out-dir wasm-out \
              target/wasm32-unknown-unknown/release/sunset_web_wasm.wasm
            mkdir -p $out
            cp wasm-out/sunset_web_wasm.js $out/
            cp wasm-out/sunset_web_wasm_bg.wasm $out/
            runHook postInstall
          '';
        };

        sunsetWebWasmPkg = mkSunsetWebWasmPkg {};

        # WASM bundle compiled with the test-hooks feature, enabling
        # voice_install_frame_recorder / voice_recorded_frames / voice_inject_pcm
        # / voice_active_peers / voice_synth_pcm for Playwright assertions.
        sunsetWebWasmTestHooksPkg = mkSunsetWebWasmPkg { features = [ "test-hooks" ]; };

        # Minimal static dist used by the voice protocol e2e tests.
        # Contains only the harness HTML, test-hooks WASM bundle, and
        # audio worklets. No full Gleam build needed.
        webVoiceTestDist = pkgs.stdenv.mkDerivation {
          name = "sunset-web-voice-test-dist";
          src = ./web;
          dontBuild = true;
          installPhase = ''
            runHook preInstall
            mkdir -p $out/audio
            cp ${./web/voice-e2e-test.html} $out/voice-e2e-test.html
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm.js $out/sunset_web_wasm.js
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm_bg.wasm $out/sunset_web_wasm_bg.wasm
            cp audio/voice-capture-worklet.js $out/audio/
            cp audio/voice-playback-worklet.js $out/audio/
            runHook postInstall
          '';
        };

        webDist = gleamLib.buildGleamPackage {
          name = "sunset-web";
          src = ./web;
          manifest = ./web/manifest.toml;
          target = "javascript";
          lustre = true;
          buildPhase = ''
            runHook preBuild
            # Link the Nix-built node_modules so Lustre's esbuild can
            # resolve bare module specifiers like `emoji-picker-element`
            # at bundle time. Without this, esbuild fails with
            # "Could not resolve: <pkg>".
            ln -sfn ${webNodeModules}/node_modules ./node_modules
            # Trigger Gleam compile to create build/dev/javascript/, then
            # stage the sunset-web-wasm bundle there so Lustre's esbuild can
            # resolve the `import init from "../../sunset_web_wasm.js"` line
            # in sunset.ffi.mjs at bundle time. (sunset.ffi.mjs gets copied
            # to build/dev/javascript/sunset_web/sunset_web/sunset.ffi.mjs;
            # the relative path resolves back up to build/dev/javascript/.)
            gleam build
            cp ${sunsetWebWasmPkg}/sunset_web_wasm.js build/dev/javascript/
            cp ${sunsetWebWasmPkg}/sunset_web_wasm_bg.wasm build/dev/javascript/
            gleam run -m lustre/dev build sunset_web --minify
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p $out
            cp -r dist/* $out/
            # Static assets (favicon, etc.) ship from web/priv. Lustre's dev
            # build doesn't copy them automatically; we do it here.
            if [ -d priv ]; then
              cp -r priv/. $out/
            fi
            # Copy the sunset-web-wasm bundle alongside the Gleam JS so
            # sunset.ffi.mjs can `import` from a relative path.
            cp ${sunsetWebWasmPkg}/sunset_web_wasm.js $out/
            cp ${sunsetWebWasmPkg}/sunset_web_wasm_bg.wasm $out/
            # Copy the C2b voice e2e test harness so Playwright can fetch it via static-web-server.
            cp ${./web/voice-e2e-test.html} $out/voice-e2e-test.html
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

        # Full Gleam UI built with the test-hooks WASM bundle. Used by Tasks 28–33
        # (real Gleam UI Playwright tests) so voice_install_frame_recorder,
        # voice_inject_pcm, voice_recorded_frames, and voice_active_peers are
        # available on window.sunsetClient when window.SUNSET_TEST=true.
        webVoiceUiTestDist = gleamLib.buildGleamPackage {
          name = "sunset-web-voice-ui-test";
          src = ./web;
          manifest = ./web/manifest.toml;
          target = "javascript";
          lustre = true;
          buildPhase = ''
            runHook preBuild
            ln -sfn ${webNodeModules}/node_modules ./node_modules
            gleam build
            # Use test-hooks WASM so voice_inject_pcm / voice_recorded_frames etc. work.
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm.js build/dev/javascript/
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm_bg.wasm build/dev/javascript/
            gleam run -m lustre/dev build sunset_web --minify
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p $out/audio/test-fixtures
            cp -r dist/* $out/
            if [ -d priv ]; then
              cp -r priv/. $out/
            fi
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm.js $out/
            cp ${sunsetWebWasmTestHooksPkg}/sunset_web_wasm_bg.wasm $out/
            cp ${./web/voice-e2e-test.html} $out/voice-e2e-test.html
            cp audio/voice-capture-worklet.js $out/audio/
            cp audio/voice-playback-worklet.js $out/audio/
            # Generate a 5-second 440 Hz sine sweep WAV for the real-mic test.
            ${pkgs.sox}/bin/sox -n -r 48000 -c 1 -e signed-integer -b 16 \
              $out/audio/test-fixtures/sweep.wav synth 5 sine 440
            ${pkgs.gnused}/bin/sed -i \
              's|src="/sunset_web\.js"|src="sunset_web.js"|' \
              $out/index.html
            touch $out/.nojekyll
            runHook postInstall
          '';
        };

        # Voice protocol e2e test runner. Uses the full Gleam UI with
        # test-hooks WASM so all voice Playwright specs (Tasks 19, 28–33)
        # work. voice_protocol.spec.js still serves from /voice-e2e-test.html
        # (included in the dist); Tasks 28–33 use the real Gleam UI at /.
        webTestRunnerVoice = pkgs.writeShellScriptBin "sunset-web-test-voice" ''
          set -euo pipefail

          export PATH="${pkgs.lib.makeBinPath [
            pkgs.nodejs
            pkgs.static-web-server
            sunsetRelayPkg
          ]}:$PATH"
          export PLAYWRIGHT_BROWSERS_PATH="${pkgs.playwright-driver.browsers}"
          export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=1
          export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
          export SUNSET_WEB_DIST="${webVoiceUiTestDist}"

          cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel)/web"
          rm -f node_modules
          ln -sfn "${webNodeModules}/node_modules" node_modules

          exec node_modules/.bin/playwright test "$@"
        '';

        # End-to-end test runner. Builds node_modules + the prod dist, then
        # invokes Playwright with browsers from the Nix store. No network.
        webTestRunner = pkgs.writeShellScriptBin "sunset-web-test" ''
          set -euo pipefail

          export PATH="${pkgs.lib.makeBinPath [
            pkgs.nodejs
            pkgs.static-web-server
            sunsetRelayPkg
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
        sunsetRelayPkg = pkgs.rustPlatform.buildRustPackage {
          pname = "sunset-relay";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "sunset-relay" "--bin" "sunset-relay" ];
          doCheck = false;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
          cargo = rustToolchain;
          rustc = rustToolchain;
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
            pkgs.static-web-server
            pkgs.wasm-bindgen-cli
            pkgs.wasm-pack
            pkgs.sox
          ];
          shellHook = ''
            ${if webHexDeps != null
              then gleamLib.devShellHook { gleamHexDeps = webHexDeps; }
              else ""}
            export PLAYWRIGHT_BROWSERS_PATH="${pkgs.playwright-driver.browsers}"
            export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=1
            export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
            # Default SUNSET_WEB_DIST to the voice-test dist (test-hooks WASM +
            # harness HTML + audio worklets). This lets `npx playwright test`
            # run voice_protocol.spec.js from the dev shell without a separate
            # `nix run .#web-test-voice` invocation. Override with the full
            # prod dist when running the entire test suite via `nix run .#web-test`.
            export SUNSET_WEB_DIST="''${SUNSET_WEB_DIST:-${webVoiceUiTestDist}}"
            # Make sunset-relay available in the dev shell for Playwright tests.
            export PATH="${pkgs.lib.makeBinPath [ sunsetRelayPkg ]}:$PATH"
          '';
        };

        packages = {
          sunset-relay = sunsetRelayPkg;
          sunset-web-wasm = sunsetWebWasmPkg;
          sunset-web-wasm-test-hooks = sunsetWebWasmTestHooksPkg;
          web-voice-test-dist = webVoiceTestDist;
          web-voice-ui-test-dist = webVoiceUiTestDist;

          sunset-relay-docker = pkgs.dockerTools.buildLayeredImage {
            name = "sunset-relay";
            tag = "latest";
            contents = [ sunsetRelayPkg pkgs.cacert ];
            config = {
              Entrypoint = [ "/bin/sunset-relay" ];
              ExposedPorts."8443/tcp" = {};
              Env = [ "RUST_LOG=sunset_relay=info" ];
              Volumes."/var/lib/sunset-relay" = {};
            };
          };

          sunset-core-wasm = pkgs.rustPlatform.buildRustPackage {
            pname = "sunset-core-wasm";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            doCheck = false;
            nativeBuildInputs = [ pkgs.wasm-bindgen-cli pkgs.lld ];
            cargo = rustToolchain;
            rustc = rustToolchain;
            # rustPlatform's cargoBuildHook hard-codes `--target <host-triple>`
            # so we sidestep it and run our own build / wasm-bindgen / install.
            buildPhase = ''
              runHook preBuild
              cargo build \
                -j $NIX_BUILD_CORES \
                --offline \
                --release \
                --target wasm32-unknown-unknown \
                -p sunset-core-wasm \
                --lib
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              wasm-bindgen \
                --target web \
                --out-dir wasm-out \
                target/wasm32-unknown-unknown/release/sunset_core_wasm.wasm
              mkdir -p $out
              cp wasm-out/sunset_core_wasm.js $out/
              cp wasm-out/sunset_core_wasm_bg.wasm $out/
              runHook postInstall
            '';
          };
        } // pkgs.lib.optionalAttrs (webHexDeps != null) {
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
          web-test-voice = {
            type = "app";
            program = "${webTestRunnerVoice}/bin/sunset-web-test-voice";
            meta.description = "Run voice protocol e2e tests (test-hooks WASM build)";
          };
        };
      });
}
