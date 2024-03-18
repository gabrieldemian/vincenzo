{
  system,
  pkgs,
  lockFile,
  fenix,
}: let
  cargoToml = builtins.fromTOML (builtins.readFile ../crates/vcz/Cargo.toml);
  toolchain = fenix.packages.${system}.minimal.toolchain;
in
  (pkgs.makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  })
  .buildRustPackage {
    pname = cargoToml.package.name;
    version = cargoToml.package.version;

    src = ../.;

    cargoLock = {
      lockFile = lockFile;
    };

    nativeBuildInputs = with pkgs; [
      pkg-config
      makeWrapper
      rustfmt
    ];

    doCheck = true;
    CARGO_BUILD_INCREMENTAL = "false";
    RUST_BACKTRACE = "full";
    copyLibs = true;

    postInstall = ''
      wrapProgram $out/bin/vcz --set RUST_BACKTRACE 1
    '';

    meta = with pkgs.lib; {
      homepage = "https://github.com/gabrieldemian/vincenzo";
      description = "Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI";
      license = licenses.mit;
      platforms = platforms.linux;
      mainProgram = "vincenzo";
    };
  }
