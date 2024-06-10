{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";

    flake-compat.url = "https://flakehub.com/f/edolstra/flake-compat/1.tar.gz";

    nix-filter.url = "github:numtide/nix-filter";
  };

  outputs =
    { self
    , crane
    , flake-utils
    , nix-filter
    , ...
    }@inputs:
    {
      overlays.default = final: prev:
        let
          inherit (final) lib stdenv darwin;
          craneLib = crane.mkLib final;
        in
        {
          myceliumd =
            let
              manifest = builtins.fromTOML (lib.readFile ./myceliumd/Cargo.toml);

              sourceRoot = "./myceliumd";
            in
            lib.makeOverridable craneLib.buildPackage {
              src = nix-filter {
                root = ./.;

                # If no include is passed, it will include all the paths.
                include = [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./mycelium
                  ./myceliumd
                  ./myceliumd-private
                  ./mobile
                ];
              };
              pname = manifest.package.name;
              inherit (manifest.package) version;

              doCheck = false;

              nativeBuildInputs = [
                final.pkg-config
                # openssl base library
                final.openssl
                # required by openssl-sys
                final.perl
              ];

              buildInputs = lib.optionals stdenv.isDarwin [
                darwin.apple_sdk.frameworks.Security
                darwin.apple_sdk.frameworks.SystemConfiguration
                final.libiconv
              ];

              meta = {
                mainProgram = "mycelium";
              };
            };
          mycelium-private =
            let
              manifest = builtins.fromTOML (lib.readFile ./myceliumd-private/Cargo.toml);

              sourceRoot = "./myceliumd-private";
            in
            lib.makeOverridable craneLib.buildPackage {
              src = nix-filter {
                root = ./.;

                include = [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./mycelium
                  ./myceliumd
                  ./myceliumd-private
                  ./mobile
                ];
              };
              pname = manifest.package.name;
              inherit (manifest.package) version;

              doCheck = false;

              nativeBuildInputs = [
                final.pkg-config
                # openssl base library
                final.openssl
                # required by openssl-sys
                final.perl
              ];

              buildInputs = lib.optionals stdenv.isDarwin [
                darwin.apple_sdk.frameworks.Security
                darwin.apple_sdk.frameworks.SystemConfiguration
                final.libiconv
              ];

              meta = {
                mainProgram = "mycelium-private";
              };
            };
        };
    } //
    flake-utils.lib.eachSystem
      [
        flake-utils.lib.system.x86_64-linux
        flake-utils.lib.system.aarch64-linux
        flake-utils.lib.system.x86_64-darwin
        flake-utils.lib.system.aarch64-darwin
      ]
      (system:
      let
        craneLib = crane.lib.${system};

        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [ self.overlays.default ];
        };
      in
      {
        devShells.default = craneLib.devShell {
          packages = [
            pkgs.rust-analyzer
          ];

          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };

        packages = {
          default = self.packages.${system}.mycelium;

          inherit (pkgs) mycelium;
        };
      });
}
