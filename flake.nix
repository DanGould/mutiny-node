{
  description = "Minimal rust wasm32-unknown-unknown example";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        isDarwin = system == "x86_64-darwin" || system == "aarch64-darwin";
         # Add firefox deps only on non-darwin.
         # darwin is listed in badPlatforms in pkgs.firefox's meta.
        optionalPackages = if isDarwin then [] else [
          pkgs.firefox
          pkgs.geckodriver
        ];
        inputs = [
          rust
          pkgs.rust-analyzer
          pkgs.openssl
          pkgs.zlib
          pkgs.gcc
          pkgs.pkg-config
          pkgs.just
          pkgs.wasm-pack
          pkgs.wasm-bindgen-cli
          pkgs.binaryen
          pkgs.clang
          pkgs.corepack_20
          pkgs.nodejs_20
        ] ++ optionalPackages;
      in
      {
        defaultPackage = pkgs.rustPlatform.buildRustPackage {
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = inputs;
        };


        devShell = pkgs.mkShell {
          packages = inputs;
          shellHook = ''
            export LIBCLANG_PATH=${pkgs.libclang.lib}/lib/
            export LD_LIBRARY_PATH=${pkgs.openssl}/lib:$LD_LIBRARY_PATH
            export CC_wasm32_unknown_unknown=${pkgs.llvmPackages_14.clang-unwrapped}/bin/clang-14
            export CFLAGS_wasm32_unknown_unknown="-I ${pkgs.llvmPackages_14.libclang.lib}/lib/clang/14.0.6/include/"
          '';
        };
      }
    );
}
