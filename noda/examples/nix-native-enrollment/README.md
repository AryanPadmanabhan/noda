# Nix-native NODA enrollment

This example shows the intended onboarding path for NixOS-managed nodes:

1. Add `noda` as a flake input.
2. Import `noda.nixosModules.noda`.
3. Enable `services.noda`.
4. Rebuild the host once.

Keep your existing hardware and host-specific modules in your own flake. `node-1.nix` only shows the NODA-specific enrollment settings.
