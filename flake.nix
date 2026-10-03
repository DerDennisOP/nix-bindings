{
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";

  outputs = {nixpkgs, ...}: let
    systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    forEachSystem = nixpkgs.lib.genAttrs systems;
    pkgsForEach = nixpkgs.legacyPackages;
    patchedNix = pkgs:
      ((pkgs.nixVersions.nixComponents_2_35.appendPatches (
          map (patch: ./nix/patches + "/${patch}") (builtins.attrNames (builtins.readDir ./nix/patches))
        )).overrideScope (finalScope: prevScope: {withAWS = false;})).nix-everything;
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
