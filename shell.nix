{ pkgs ? import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/313b84933167.tar.gz") {
    overlays = [
      (import (fetchTarball "https://github.com/oxalica/rust-overlay/archive/d0dc81ffe8ea.tar.gz"))
    ];
  }
}:

let
  rust = with pkgs;
    rust-bin.stable.latest.minimal;
  basePkgs = with pkgs;
    [
      cmake
      rust
      act
      cargo-zigbuild
    ];

  # macOS only
  inputs = with pkgs;
    basePkgs ++ lib.optionals stdenv.isDarwin
      (with darwin.apple_sdk.frameworks; [
        Security
      ]);
in
pkgs.mkShell
{
  buildInputs = inputs;
}
