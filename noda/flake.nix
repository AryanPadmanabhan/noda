{
  description = "noda: OTA orchestrator and NixOS agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages.noda = pkgs.rustPlatform.buildRustPackage {
          pname = "noda";
          version = "0.2.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        packages.default = self.packages.${system}.noda;

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.noda}/bin/noda";
        };
      }) // {
        nixosModules.noda = import ./nix/modules/noda-agent.nix;
        nixosModules.noda-server = import ./nix/modules/noda-server.nix;
      };
}
