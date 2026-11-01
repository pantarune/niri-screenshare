{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.niri-screenshare;
in
{
  options.services.niri-screenshare = {
    enable = lib.mkEnableOption "the niri ScreenCast portal";

    withPicker = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Build the default package with the GTK display and window picker.";
    };

    dynamicCast = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Use Niri's Dynamic Cast Target instead of selecting a fixed source.
        This bypasses the picker and lets Niri's set-dynamic-cast-window and
        set-dynamic-cast-monitor actions change the stream's target.
      '';
    };

    dynamicCastAutoSelectMonitor = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Select the focused monitor when starting a dynamic cast. This also
        changes the target of every existing dynamic stream.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { inherit (cfg) withPicker; };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { inherit (cfg) withPicker; }";
      description = "The niri-screenshare package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    xdg.portal = {
      enable = true;
      extraPortals = [ cfg.package ];
      config.niri."org.freedesktop.impl.portal.ScreenCast" = "niri";
    };

    systemd.user.services.niri-screenshare = {
      enableDefaultPath = false;
      environment.NIRI_SCREENSHARE_NO_PORTAL_CONFIG = "1";
      environment.NIRI_SCREENSHARE_DYNAMIC_CAST = lib.mkIf cfg.dynamicCast "1";
      environment.NIRI_SCREENSHARE_DYNAMIC_CAST_AUTO_SELECT_MONITOR = lib.mkIf cfg.dynamicCastAutoSelectMonitor "1";
    };
  };
}
