{
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";
  inputs.nix-fork.url = "git+file:///home/dennis/repos/nix?ref=feat/optimized-eval-capi";
  inputs.nix-fork.inputs.nixpkgs.follows = "nixpkgs";

  outputs = {nixpkgs, nix-fork, ...}: let
    systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    forEachSystem = nixpkgs.lib.genAttrs systems;
    pkgsForEach = nixpkgs.legacyPackages;
  in {
    devShells = forEachSystem (system: let
      pkgs = pkgsForEach.${system};
    in {
      default = pkgsForEach.${system}.callPackage ./shell.nix {
          inherit pkgs;
          nixForBindings = nix-fork.packages.${system}.default;
        };
    });
  };
}
