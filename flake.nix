{
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";

  outputs = {nixpkgs, ...}: let
    systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    forEachSystem = nixpkgs.lib.genAttrs systems;
    pkgsForEach = nixpkgs.legacyPackages;
    nixVersion = "2.35.3";
    patchedNix = pkgs: let
      src = pkgs.fetchFromGitHub {
        owner = "NixOS";
        repo = "nix";
        tag = nixVersion;
        hash = "sha256-qtlx8ywd1qj2ToZTNBunXE7JUajLtw44ja3r7+d47D4=";
      };
    in
      (((pkgs.nixVersions.nixComponents_2_35.overrideSource src).appendPatches (
          map (patch: ./nix/patches + "/${patch}") (builtins.attrNames (builtins.readDir ./nix/patches))
        )).overrideScope (finalScope: prevScope: {version = nixVersion; withAWS = false;})).nix-everything;
  in {
    devShells = forEachSystem (system: let
      pkgs = pkgsForEach.${system};
    in {
      default = pkgs.callPackage ./shell.nix {
        inherit pkgs;
        nixForBindings = patchedNix pkgs;
      };
    });
  };
}
