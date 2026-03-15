{ self, pkgs, ... }:
{
  imports = [
    ./hardware-configuration.nix
  ];

  system.stateVersion = "25.11";

  nix.settings.experimental-features = [ "nix-command" "flakes" ];

  boot.loader.grub = {
    enable = true;
    efiSupport = true;
    device = "nodev";
  };

  boot.loader.efi.canTouchEfiVariables = true;
  boot.loader.efi.efiSysMountPoint = "/boot";

  networking.hostName = "ota-vm";
  networking.networkmanager.enable = true;

  time.timeZone = "America/Chicago";

  environment.systemPackages = [ pkgs.tree ];
  environment.etc."deploy-intent/baseline-release".text = "baseline";

  services.openssh.enable = true;
  services.openssh.settings.PasswordAuthentication = true;

  services.deploy-intent-agent = {
    enable = true;
    package = self.packages.${pkgs.system}.deploy-intent;
    serverUrl = "http://10.2.24.81:8080";
    assetId = "nix-vm-1";
    assetType = "edge-linux-aarch64";
    missionState = "idle";
    pollSeconds = 15;
    stateDir = "/var/lib/deploy-intent";
    labels = [ "region=lab" ];
  };
}
