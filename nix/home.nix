# Home Manager module: keeps every harness's own files under ~/.ai/harness with `ai harness`.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.ai-harness;
  ai = lib.getExe cfg.package;
in
{
  options.services.ai-harness = {
    enable = lib.mkEnableOption "keeping every harness's own files under ~/.ai/harness";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.ai;
      defaultText = lib.literalExpression "ai-tools.packages.\${system}.ai";
      description = "The ai that runs `ai harness`.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Never moves data, so a switch never copies while harnesses run; a failed link leaves the
    # folders where they are and the rest of the switch goes on.
    home.activation.aiHarness = lib.hm.dag.entryAfter [ "linkGeneration" ] ''
      run ${ai} harness link || errorEcho "ai harness link failed; harness files stay where they are."
    '';

    # Started with the user manager at boot, before any login, so no harness has started yet and
    # no login waits for it. `ai harness move` itself leaves any folder in use where it is.
    systemd.user.services.ai-harness = {
      Unit = {
        Description = "Keep every harness's own files under ~/.ai/harness";
        # Harnesses that run as services start after it.
        Before = [ "hermes-gateway.service" ];
        X-SwitchMethod = "keep-old";
      };
      Service = {
        Type = "oneshot";
        ExecStart = "${ai} harness move";
        # The first move copies and hashes every harness folder once; a move cut off is redone
        # at the next boot.
        TimeoutStartSec = "15min";
      };
      Install.WantedBy = [ "default.target" ];
    };
  };
}
