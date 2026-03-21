{ noda, pkgs, ... }:
{
  # Keep your host's existing hardware config and other host-specific modules.
  imports = [ ];

  services.noda = {
    enable = true;
    package = noda.packages.${pkgs.system}.noda;
    serverUrl = "http://10.2.24.81:8080";
    assetId = "node-1";
    assetType = "edge-linux-aarch64";
    missionState = "idle";
    labels = [ "region=lab" ];
  };
}
