{
  description = "jobpipe — daily ranked digest of new job postings from company ATS boards";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      forAllSystems = f: builtins.listToAttrs (map
        (system: { name = system; value = f system; })
        systems);
    in
    {
      # `nix build` / `nix run github:you/jobpipe` builds the CLI.
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # Minimal stable toolchain (rustc + cargo + std) — no docs/clippy/etc.,
          # which keeps the release build lean.
          toolchain = fenix.packages.${system}.stable.minimalToolchain;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          jobpipe = rustPlatform.buildRustPackage {
            pname = "jobpipe";
            version = "1.0.3";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # libsqlite3-sys links SQLite; reqwest uses rustls (no OpenSSL needed).
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.sqlite ];
            # The test suite would need a live DB / network; skip in the sandbox.
            doCheck = false;
          };
        in
        {
          jobpipe = jobpipe;
          default = jobpipe;
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/jobpipe";
        };
      });

      devShells = forAllSystems (system:
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
          # `jobpipe ...` in the dev shell runs the optimized (release) build,
          # rebuilding incrementally only when the source changed. cargo finds the
          # manifest by searching up from the current directory, so this works
          # anywhere inside the project tree.
          jobpipe-dev = pkgs.writeShellScriptBin "jobpipe" ''
            exec cargo run --release --quiet -- "$@"
          '';
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              toolchain
              sqlite
              pkg-config
              git
              jobpipe-dev
            ];

            RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          };
        });
    };
}
