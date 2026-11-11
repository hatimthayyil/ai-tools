# Home Manager module: backs up every harness's transcripts with `ai backup`
# (services.ai-tools.backup).
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.ai-tools;
in
{
  options.services.ai-tools = {
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.ai;
      defaultText = lib.literalExpression "ai-tools.packages.\${system}.ai";
      description = "The ai that runs ai-tools' services.";
    };

    backup.enable = lib.mkEnableOption "backing up every harness's transcripts with restic, again and again, as [backup] in ~/.ai/ai.toml sets it";
  };

  config = lib.mkIf cfg.backup.enable {
    # Outside every sandbox. Started with the user manager, so it runs from boot when the user
    # lingers, else from the first login. It reads [backup] before each run, so a change to
    # the settings needs no restart.
    systemd.user.services.ai-tools-backup = {
      Unit.Description = "Back up every harness's transcripts with restic";
      Service = {
        ExecStart = "${lib.getExe cfg.package} backup watch";
        # A run cut short loses nothing; the next one backs up what it left. Broken settings
        # end the service, which then tries again.
        Restart = "on-failure";
        RestartSec = "30s";
        Nice = 10;
      };
      Install.WantedBy = [ "default.target" ];
    };
  };
}
