{
  description = "Sunset - a Lustre Gleam application";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    relay.url = "path:./relay";
  };

  outputs = { self, nixpkgs, flake-utils, relay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        fs = pkgs.lib.fileset;
        gleamLib = import ./nix/gleam { inherit pkgs; };

        relayPkg = relay.packages.${system}.default;

        gleamHexDeps = gleamLib.fetchHexDeps { manifest = ./manifest.toml; };

        npmSrc = fs.toSource {
          root = ./.;
          fileset = fs.unions [ ./package.json ./package-lock.json ];
        };

        appNpmDeps = pkgs.fetchNpmDeps {
          src = npmSrc;
          hash = "sha256-0/JUUKtgk4rcGXkjgVLClK3BImURfh7DolkiiThpZS0=";
        };

        mkNodeModules = { name, src, npmDeps }: pkgs.stdenv.mkDerivation {
          inherit name src;
          nativeBuildInputs = [ pkgs.nodejs pkgs.npmHooks.npmConfigHook ];
          inherit npmDeps;
          npmRebuildFlags = [ "--ignore-scripts" ];
          dontBuild = true;
          installPhase = "mkdir -p $out && cp -r node_modules $out/node_modules";
        };

        appNodeModules = mkNodeModules {
          name = "sunset-app-node-modules";
          src = npmSrc;
          npmDeps = appNpmDeps;
        };

        devSetupScript = ''
          ${gleamLib.devShellHook { inherit gleamHexDeps; }}
          if [ ! -d node_modules ] || [ -L node_modules ]; then
            ln -sfn ${appNodeModules}/node_modules node_modules
          fi
        '';

        sunsetDist = gleamLib.buildGleamPackage {
          name = "sunset-dist";
          src = fs.toSource {
            root = ./.;
            fileset = fs.gitTracked ./.;
          };
          manifest = ./manifest.toml;
          target = "javascript";
          lustre = true;
          nativeBuildInputs = [ pkgs.npmHooks.npmConfigHook ];
          npmDeps = appNpmDeps;
          npmRebuildFlags = [ "--ignore-scripts" ];
          buildPhase = ''
            runHook preBuild
            gleam run -m lustre/dev build sunset --minify
            runHook postBuild
          '';
          installPhase = "cp -r dist $out";
        };

        testNpmDeps = pkgs.fetchNpmDeps {
          src = fs.toSource {
            root = ./test/integration;
            fileset = fs.unions [
              ./test/integration/package.json
              ./test/integration/package-lock.json
            ];
          };
          hash = "sha256-DPd/VwddsMMvA/tF0lzsQYR99OR8cSYISYqFt6aYeYA=";
        };

        testNodeModules = mkNodeModules {
          name = "sunset-test-node-modules";
          src = ./test/integration;
          npmDeps = testNpmDeps;
        };

        integrationTestSrc = pkgs.runCommand "sunset-integration-test-src" {} ''
          mkdir -p $out
          for f in ${./test/integration}/*.mjs ${./test/integration}/package.json ${./test/integration}/test-tone.wav; do
            cp "$f" $out/
          done
          ln -s ${testNodeModules}/node_modules $out/node_modules
        '';

      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [ gleam erlang rebar3 nodejs bun inotify-tools ];
          shellHook = devSetupScript;
        };

        packages = {
          default = pkgs.writeShellScriptBin "sunset" ''
            export SERVER_HOST="''${SERVER_HOST:-127.0.0.1}"
            export SERVER_PORT="''${SERVER_PORT:-8080}"
            exec ${pkgs.static-web-server}/bin/static-web-server \
              --root ${sunsetDist} "$@"
          '';
          dev = pkgs.writeShellScriptBin "sunset-dev" ''
            export PATH="${pkgs.lib.makeBinPath (with pkgs; [ gleam erlang rebar3 nodejs bun inotify-tools ])}:$PATH"
            cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel 2>/dev/null || echo .)"
            ${devSetupScript}
            exec ${pkgs.gleam}/bin/gleam run -m lustre/dev start
          '';
          inherit sunsetDist;
          relay = relayPkg;
        };

        apps = {
          default = { type = "app"; program = "${self.packages.${system}.default}/bin/sunset"; meta.description = "Sunset production server"; };
          dev = { type = "app"; program = "${self.packages.${system}.dev}/bin/sunset-dev"; meta.description = "Sunset dev server with live reload"; };
          integration-test = {
            type = "app";
            program = "${pkgs.writeShellScript "integration-test" ''
              set -euo pipefail
              export PATH="${pkgs.lib.makeBinPath [ pkgs.nodejs ]}:$PATH"
              export SUNSET_DIST_DIR="${sunsetDist}"
              export SUNSET_RELAY_BIN="${relayPkg}/bin/relay"
              cd ${integrationTestSrc}
              exec ${pkgs.nodejs}/bin/node --test --test-reporter=spec --test-concurrency=2 --test-timeout=120000 "$@"
            ''}";
            meta.description = "Run integration tests locally (no VM)";
          };
        };

        checks.unit = gleamLib.buildGleamPackage {
          name = "sunset-unit-tests";
          src = fs.toSource {
            root = ./.;
            fileset = fs.gitTracked ./.;
          };
          manifest = ./manifest.toml;
          target = "javascript";
          nativeBuildInputs = [ pkgs.npmHooks.npmConfigHook ];
          npmDeps = appNpmDeps;
          npmRebuildFlags = [ "--ignore-scripts" ];
          buildPhase = ''
            runHook preBuild
            gleam test
            runHook postBuild
          '';
          installPhase = "touch $out";
        };

        checks.integration = pkgs.testers.nixosTest {
          name = "sunset-integration";

          nodes.machine = { pkgs, ... }: {
            virtualisation = { memorySize = 8192; diskSize = 4096; cores = 4; };
            environment.systemPackages = with pkgs; [ chromium nodejs curl ];
            networking.firewall.enable = false;
          };

          testScript = ''
            machine.wait_for_unit("multi-user.target")

            machine.succeed(
              "cd ${integrationTestSrc} && "
              "SUNSET_DIST_DIR=${sunsetDist} "
              "SUNSET_RELAY_BIN=${relayPkg}/bin/relay "
              "PUPPETEER_EXECUTABLE_PATH=${pkgs.chromium}/bin/chromium "
              "node --test --test-concurrency=1 --test-timeout=120000 *.test.mjs "
              "2>&1 | tee /tmp/test-output.log"
            )
          '';
        };
      }
    );
}
