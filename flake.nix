{
  description = "flake for rust projects";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      systems = [ "x86_64-linux" ];
    in
    {
      devShells = builtins.listToAttrs (map
        (system: {
          name = system;
          value =
            let
              pkgs = nixpkgs.legacyPackages.${system};
              toolchain = fenix.packages.${system}.stable.withComponents [
                "rustc"
                "cargo"
                "clippy"
                "rustfmt"
                "rust-src"
                "rust-analyzer"
              ];
            in
            {
              default = pkgs.mkShell {
                buildInputs = with pkgs; [
                  toolchain
                  postgresql
                  openssl
                  openssl.dev
                  pkg-config
                  git
                ];

                allowUnfree = true;

                RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";

                shellHook = ''
                  export LD_LIBRARY_PATH="${pkgs.openssl.out}/lib:$LD_LIBRARY_PATH"
                  export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig"
                  export PGDATA=$PWD/.pgdata
                  export PGHOST=$PWD/.pgsocket
                  mkdir -p $PGHOST
                  if [ ! -d "$PGDATA" ]; then
                    initdb --auth=trust --no-locale --encoding=UTF8
                  fi
                '';
              };
            };
        })
        systems);
    };
}

