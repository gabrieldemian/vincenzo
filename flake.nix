{
  description = "Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    systems.url = "github:nix-systems/default-linux";
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
    systems,
    ...
  } @ inputs: let
    inherit (nixpkgs) lib;
    eachSystem = lib.genAttrs (import systems);
    pkgsFor = eachSystem (
      system: let
        systemPkgs = import nixpkgs {
          inherit system;
          overlays = [
            fenix.overlays.default
          ];
        };
      in
        systemPkgs
        // {
          vincenzo = import ./nix/default.nix {
            inherit system;
            pkgs = systemPkgs;
            lockFile = ./Cargo.lock;
            fenix = fenix;
          };
        }
    );
  in {
    packages = eachSystem (system: {
      vincenzo = pkgsFor.${system}.vincenzo;
    });

    homeManagerModules = {
      vincenzo = import ./nix/hm-module.nix self;
    };

    checks = eachSystem (system: self.packages.${system});

    formatter = eachSystem (system: pkgsFor.${system}.alejandra);
  };
}
