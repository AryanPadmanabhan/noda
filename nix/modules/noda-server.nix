{ lib, config, ... }:
let
  cfg = config.services.noda-server;
  inherit (lib) escapeShellArg mkEnableOption mkIf mkOption types;
in
{
  options.services.noda-server = {
    enable = mkEnableOption "NODA control-plane server";

    package = mkOption {
      type = types.package;
      description = "Package providing the NODA server binary.";
    };

    bind = mkOption {
      type = types.str;
      default = "0.0.0.0:8080";
      description = "Bind address for the NODA HTTP API.";
    };

    dataDir = mkOption {
      type = types.str;
      default = "/var/lib/noda-server";
      description = "Persistent data directory for the NODA server.";
    };

    dbPath = mkOption {
      type = types.str;
      default = "/var/lib/noda-server/noda.db";
      description = "SQLite database path for the NODA server.";
    };

    extraArgs = mkOption {
      type = types.str;
      default = "";
      description = "Additional raw arguments appended to the server command.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.package != null;
        message = "services.noda-server.package must be set.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d ${cfg.dataDir} 0750 root root -"
    ];

    systemd.services.noda-server = {
      description = "node over-the-air control plane";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      restartIfChanged = true;
      serviceConfig = {
        Type = "simple";
        ExecStart = ''
          ${cfg.package}/bin/noda server \
            --bind ${escapeShellArg cfg.bind} \
            --db ${escapeShellArg cfg.dbPath} \
            ${cfg.extraArgs}
        '';
        Restart = "always";
        RestartSec = 5;
        WorkingDirectory = cfg.dataDir;
        User = "root";
        Environment = [
          "PATH=/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin"
        ];
      };
    };
  };
}
