{
  description = "Minimal Nix-native NODA enrollment example";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    noda.url = "path:../..";
  };

  outputs = { self, nixpkgs, noda, ... }: {
    nixosConfigurations.node-1 = nixpkgs.lib.nixosSystem {
      system = "aarch64-linux";
      specialArgs = { inherit noda; };
      modules = [
        ./node-1.nix
        noda.nixosModules.noda
      ];
    };
  };
}
