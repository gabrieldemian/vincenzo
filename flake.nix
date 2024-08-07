{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in
        with pkgs; {
          devShells.default = mkShell {
            shellHook = ''
              export XDG_DOWNLOAD_DIR="$HOME/Downloads";
              export XDG_CONFIG_HOME="$HOME/.config";
              export XDG_STATE_HOME="$HOME/.local/state";
              export XDG_DATA_HOME="$HOME/.local/share";
            '';
            buildInputs = [
              taplo
              pkg-config
              glib
              (
                rust-bin.selectLatestNightlyWith (toolchain:
                  toolchain.default.override {
                    extensions = ["rust-src"];
                  })
              )
            ];
          };
        }
    );
}
