{ lib, pkgs, config, ... }:
let
  cfg = config.services.noda;
  inherit (lib) concatStringsSep escapeShellArg mkEnableOption mkIf mkOption types;
  labelArgs = concatStringsSep " " (map (label: "--labels ${escapeShellArg label}") cfg.labels);
  startScript = pkgs.writeShellScript "noda-start" ''
    exec ${cfg.package}/bin/noda agent \
      --server ${escapeShellArg cfg.serverUrl} \
      --asset-id ${escapeShellArg cfg.assetId} \
      --asset-type ${escapeShellArg cfg.assetType} \
      --mission-state ${escapeShellArg cfg.missionState} \
      --poll-seconds ${toString cfg.pollSeconds} \
      --state-dir ${escapeShellArg cfg.stateDir} \
      ${labelArgs} \
      ${cfg.extraArgs}
  '';
in
{
  options.services.noda = {
    enable = mkEnableOption "NODA polling agent";

    package = mkOption {
      type = types.package;
      description = "Package providing the NODA agent binary.";
    };

    serverUrl = mkOption {
      type = types.str;
      description = "Control-plane base URL.";
    };

    assetId = mkOption {
      type = types.str;
      description = "Unique asset identity reported by the agent.";
    };

    assetType = mkOption {
      type = types.str;
      default = "edge-linux-aarch64";
      description = "Asset type used for deployment target matching.";
    };

    missionState = mkOption {
      type = types.str;
      default = "idle";
      description = "Mission state reported on check-in.";
    };

    pollSeconds = mkOption {
      type = types.ints.positive;
      default = 15;
      description = "Polling interval in seconds.";
    };

    stateDir = mkOption {
      type = types.str;
      default = "/var/lib/noda";
      description = "Persistent state directory for downloads and reboot-resume state.";
    };

    labels = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Key=value labels advertised by the agent.";
    };

    extraArgs = mkOption {
      type = types.str;
      default = "";
      description = "Additional raw arguments appended to the agent command.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.package != null;
        message = "services.noda.package must be set.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d ${cfg.stateDir} 0750 root root -"
    ];

    systemd.services.noda = {
      description = "node over-the-air agent";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      restartIfChanged = true;
      serviceConfig = {
        Type = "simple";
        ExecStart = startScript;
        Restart = "always";
        RestartSec = 5;
        WorkingDirectory = cfg.stateDir;
        User = "root";
        Environment = [
          "PATH=/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin"
        ];
      };
    };
  };
}
