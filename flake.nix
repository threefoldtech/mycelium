{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";

    flake-utils.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
  }:
    flake-utils.lib.eachSystem
    [
      flake-utils.lib.system.x86_64-linux
      flake-utils.lib.system.aarch64-linux
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

      packages.default = craneLib.buildPackage {
        src = self;
        pname = manifest.package.name;
        version = manifest.package.version;

        doCheck = true;

        cargoExtraArgs = "--bin ${manifest.package.name}";

        buildInputs = lib.optionals stdenv.isDarwin [
          darwin.apple_sdk.frameworks.Security
          darwin.apple_sdk.frameworks.SystemConfiguration
        ];
      };
    });
}
