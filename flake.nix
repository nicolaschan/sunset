{
  description = "Sutro - a Lustre Gleam application";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            gleam
            erlang
            rebar3
            nodejs
            inotify-tools
          ];

          shellHook = ''
            echo "sutro dev shell"
            echo "  gleam $(gleam --version)"
            echo "  node $(node --version)"
            echo ""
            echo "Commands:"
            echo "  gleam run -m lustre/dev start  # Start dev server"
            echo "  gleam build                    # Build the project"
            echo "  gleam test                     # Run tests"
          '';
        };

        packages.default = pkgs.writeShellScriptBin "sutro-dev" ''
          export PATH="${pkgs.lib.makeBinPath (with pkgs; [ gleam erlang rebar3 nodejs inotify-tools ])}:$PATH"
          cd "$(${pkgs.git}/bin/git rev-parse --show-toplevel 2>/dev/null || echo .)"
          echo "Starting Lustre dev server..."
          exec ${pkgs.gleam}/bin/gleam run -m lustre/dev start
        '';

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/sutro-dev";
        };
      }
    );
}
