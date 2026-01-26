{ pkgs ? import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/313b84933167.tar.gz") {
    overlays = [
      (import (fetchTarball "https://github.com/oxalica/rust-overlay/archive/577ee84c69ba.tar.gz"))
    ];
  }
}:

let
  basePkgs = with pkgs;
    [
      cmake
      rustup
      act
      cargo-zigbuild
      cargo-release
    ];

  # macOS only
  inputs = with pkgs;
    basePkgs ++ lib.optionals stdenv.isDarwin
      (with darwin.apple_sdk.frameworks; [
        Security
        SystemConfiguration
        CoreServices
      ]);
in
pkgs.mkShell
{
  buildInputs = inputs;
}
