self: {
  config,
  pkgs,
  lib,
  ...
}: let
  inherit (lib.types) package str;
  inherit (lib.modules) mkIf;
  inherit (lib.options) mkOption mkEnableOption;

  cfg = config.programs.vincenzo;
  filterOptions = options:
    builtins.filter (opt: builtins.elemAt opt 1 != "") options;
in {
  options.programs.vincenzo = {
    enable =
      mkEnableOption ""
      // {
        description = ''
          Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI.
        '';
      };

    package = mkOption {
      description = "The vincenzo package";
      type = package;
      default = self.package.${pkgs.stdenv.hostPlatform.system}.vincenzo;
    };
    download_dir = mkOption {
      description = "The directory where Vincenzo will download files to.";
      type = str;
      default = "${config.xdg.userDirs.downloads}/";
    };
    daemon_addr = mkOption {
      description = "The address of the daemon to connect to.";
      type = str;
    };
  };

  config = mkIf cfg.enable {
    home.packages = [cfg.package];
    xdg.configFile."vincenzo/config.toml".text = let
      formatOption = name: value: "${name}=${value}";
      formatConfig = options:
        builtins.concatStringsSep "\n" (map (opt: formatOption (builtins.head opt) (builtins.elemAt opt 1)) options);
    in ''
      ${formatConfig (filterOptions [
        ["download_dir" (cfg.download_dir)]
        ["daemon_addr" (cfg.daemon_addr)]
      ])}
    '';
  };
}
