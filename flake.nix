{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";

    flake-utils.inputs.nixpkgs.follows = "nixpkgs";

    flake-compat.url = "https://flakehub.com/f/edolstra/flake-compat/1.tar.gz";

    nix-filter.url = "github:numtide/nix-filter";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    nix-filter,
    ...
  }:
    flake-utils.lib.eachSystem
    [
      flake-utils.lib.system.x86_64-linux
      flake-utils.lib.system.aarch64-linux
      flake-utils.lib.system.x86_64-darwin
      flake-utils.lib.system.aarch64-darwin
    ] (system: let
      craneLib = crane.lib.${system};

      pkgs = nixpkgs.legacyPackages.${system};
      inherit
        (pkgs)
        lib
        stdenv
        darwin
        ;

      manifest = builtins.fromTOML (builtins.readFile ./mycelium/Cargo.toml);
      sourceInfo =
        self.sourceInfo
        // {
          packageVersion = manifest.package;
        };
    in {
      devShells.default = craneLib.devShell {
        packages = [
          pkgs.rust-analyzer
        ];

        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };

      packages = {
        default = self.packages.${system}.mycelium;
        mycelium = lib.makeOverridable craneLib.buildPackage {
          src = nix-filter {
            root = ./.;

            # If no include is passed, it will include all the paths.
            include = [
              ./Cargo.toml
              ./Cargo.lock
              ./mycelium
              ./myceliumd
            ];
          };
          pname = manifest.package.name;
          version = manifest.package.version;

          doCheck = false;

          cargoExtraArgs = "--bin mycelium";

          nativeBuildInputs = [
            # required by openssl-sys
            pkgs.perl
          ];

          buildInputs = lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];
        };
      };
    });
}
