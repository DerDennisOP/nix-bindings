{
  pkgs ? import <nixpkgs> {},
  # Supplied by flake.nix from the local nix fork; falls back to nixpkgs for bare `nix-shell` use.
  nixForBindings ? pkgs.nixVersions.nix_2_34,
}: let
  inherit (pkgs.rustc) llvmPackages;
in
  pkgs.mkShell {
    name = "nix-bindings";

    strictDeps = true;
    nativeBuildInputs = with pkgs; [
      pkg-config
      cargo
      rustc
      llvmPackages.lld

      (rustfmt.override {asNightly = true;})
      rust-analyzer-unwrapped
      clippy
      taplo
      lldb

      # Additional Cargo tooling
      cargo-llvm-cov
      cargo-nextest
    ];

    buildInputs = [
      nixForBindings.dev
      pkgs.glibc.dev
    ];

    env = let
      inherit (llvmPackages) llvm;
    in {
      RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
      BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${pkgs.glibc.dev}";

      # `cargo-llvm-cov` reads these environment variables to find these binaries,
      # which are needed to run the tests
      LLVM_COV = "${llvm}/bin/llvm-cov";
      LLVM_PROFDATA = "${llvm}/bin/llvm-profdata";
    };
  }
