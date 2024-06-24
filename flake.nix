{
  description = "Interact with Railway via CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/22.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs = { self, rust-overlay, nixpkgs, crane, flake-utils, advisory-db, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        toolchain = pkgs.rust-bin.stable.latest.default;

        inherit (pkgs) lib;

        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
        src =
          let
            # Only keeps graphql files
            markdownFilter = path: _type: builtins.match ".*graphql$" path != null;
            markdownOrCargo = path: type:
              (markdownFilter path type) || (craneLib.filterCargoSources path type);
          in
          lib.cleanSourceWith {
            src = ./.;
            filter = markdownOrCargo;
          };

        # Common arguments can be set here to avoid repeating them later
        commonArgs = {
          inherit src;
          pname = "railway";
          buildInputs = [
            # Add additional build inputs here
          ] ++ lib.optionals pkgs.stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            pkgs.libiconv
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];

          # Additional environment variables can be set directly
          # MY_CUSTOM_VAR = "some value";
        };

        # Build *just* the cargo dependencies, so we can reuse
        # all of that work (e.g. via cachix) when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the dependency
        # artifacts from above.
        railway = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        clippy = craneLib.cargoClippy (commonArgs // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- --deny warnings";
        });

        audit = craneLib.cargoAudit (commonArgs // {
          inherit advisory-db;
        });

        fmt = craneLib.cargoFmt (commonArgs // { });

        test = craneLib.cargoNextest (commonArgs // {
          inherit cargoArtifacts;
          partitions = 1;
          partitionType = "count";
        });
      in
      {
        checks = { };

        packages = {
          default = railway;
          inherit clippy audit fmt test;
        };

        apps = {
          default = flake-utils.lib.mkApp {
            drv = railway;
          };

          clippy = flake-utils.lib.mkApp {
            drv = clippy;
          };

          audit = flake-utils.lib.mkApp {
            drv = audit;
          };

          fmt = flake-utils.lib.mkApp {
            drv = fmt;
          };

          test = flake-utils.lib.mkApp {
            drv = test;
          };
        };

        devShells.default =
          import ./shell.nix {
            inherit pkgs;
          };
      });
}
