{
  description = "Budget-guarded Google Ads reporting and optimization MCP server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      mkGoogleAdsMcp = pkgs:
        let cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          version = cargoToml.package.version;

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
            ];
          };

          cargoLock.lockFile = ./Cargo.lock;

          meta = with pkgs.lib; {
            description = cargoToml.package.description;
            license = licenses.mit;
            mainProgram = "google-ads-mcp";
            platforms = platforms.unix;
          };
        };

      overlay = final: _prev: {
        google-ads-mcp = mkGoogleAdsMcp final;
      };
    in
    flake-utils.lib.eachDefaultSystem
      (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          packages.google-ads-mcp = mkGoogleAdsMcp pkgs;
          packages.default = self.packages.${system}.google-ads-mcp;

          devShells.default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.google-ads-mcp ];
            packages = with pkgs; [ cargo rustc rust-analyzer clippy rustfmt ];
          };

          formatter = pkgs.nixpkgs-fmt;
        })
    // {
      overlays.default = overlay;
    };
}
