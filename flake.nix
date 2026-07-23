{
  description = "Portal backend for niri implementing ScreenCast";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
      ...
    }:
    let
      eachSystem = nixpkgs.lib.genAttrs nixpkgs.lib.platforms.linux;
      pkgsFor = eachSystem (
        system:
        nixpkgs.legacyPackages.${system}.appendOverlays [
          self.overlays.default
        ]
      );
    in
    {
      formatter = eachSystem (system: pkgsFor.${system}.nixfmt);

      overlays.default = final: _prev: {
        niri-screenshare = final.callPackage ./nix/package.nix { };
      };

      packages = eachSystem (system: {
        default = pkgsFor.${system}.niri-screenshare;
        niri-screenshare = pkgsFor.${system}.niri-screenshare;
      });

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.callPackage ./nix/shell.nix { };
      });
    };
}
