# Home Manager module for Cuetty's global configuration.
#
# The global package lives in `$XDG_CONFIG_HOME/cuetty` and is CUE-unified
# beneath each directory's `package cuetty`. Concrete global values would
# conflict with any directory that sets a different value, so every scalar in
# `settings` is rendered as a CUE default (`*value | type`) that directories
# can override.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.cuetty;

  cueType =
    value:
    if builtins.isBool value then
      "bool"
    else if builtins.isInt value then
      "int"
    else if builtins.isFloat value then
      "number"
    else
      "string";

  toCue =
    indent: value:
    if builtins.isAttrs value then
      "{\n" + fields "${indent}  " value + "${indent}}"
    else if builtins.isList value then
      builtins.toJSON value
    else
      "*${builtins.toJSON value} | ${cueType value}";

  fields =
    indent: attrs:
    lib.concatStrings (
      lib.mapAttrsToList (name: value: "${indent}${builtins.toJSON name}: ${toCue indent value}\n") attrs
    );

  configText = lib.concatStringsSep "\n" (
    [ "package cuetty\n" ]
    ++ lib.optional (cfg.settings != { }) (fields "" cfg.settings)
    ++ lib.optional (cfg.extraConfig != "") cfg.extraConfig
  );
in
{
  options.programs.cuetty = {
    enable = lib.mkEnableOption "Cuetty, cuenv's GPUI terminal";

    package = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.cuetty or null;
      defaultText = lib.literalExpression "cuenv.packages.\${system}.cuetty";
      description = "The Cuetty package to install. Set to null to manage configuration only.";
    };

    settings = lib.mkOption {
      type =
        let
          leaf = lib.types.oneOf [
            lib.types.bool
            lib.types.int
            lib.types.float
            lib.types.str
          ];
        in
        lib.types.attrsOf (lib.types.either leaf (lib.types.attrsOf leaf));
      default = { };
      example = lib.literalExpression ''
        {
          rail = "#89b4fa";
          titlebar = false;
          keybindings.palette = "ctrl-shift-p";
        }
      '';
      description = ''
        Global `package cuetty` values written to
        {file}`$XDG_CONFIG_HOME/cuetty/config.cue`. Each value is rendered as a
        CUE default so per-directory packages can override it.
      '';
    };

    extraConfig = lib.mkOption {
      type = lib.types.lines;
      default = "";
      example = ''
        banner: *"home" | string
      '';
      description = "Raw CUE appended to the global package after `settings`.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.optional (cfg.package != null) cfg.package;
    xdg.configFile."cuetty/config.cue".text = configText;
  };
}
