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


# /dev/vda4: UUID="48ac5b68-981a-4118-ad21-fc67a5dc6210" BLOCK_SIZE="4096" TYPE="ext4" PARTUUID="00fb4d02-dd30-46a0-ad33-821d9cd13062"
# /dev/vda5: LABEL="persist" UUID="cf906e9a-c859-4742-a399-d2744a9a8741" BLOCK_SIZE="4096" TYPE="ext4" PARTUUID="7e4a7e3a-84e3-4c6d-9aaa-119a921f1a01"
# /dev/vda3: LABEL="rootfs-a" UUID="4f6004ef-535d-4883-86be-841856e227ad" BLOCK_SIZE="4096" TYPE="ext4" PARTUUID="ba4e5af1-2c29-49a2-8feb-c01b81b1894b"
# /dev/vda1: UUID="BDB5-C815" BLOCK_SIZE="512" TYPE="vfat" PARTUUID="66a1d479-ef07-4c73-8356-77cbedc7117b"

vmlinuz-6.8.0-106-generic
initrd.img-6.8.0-106-generic