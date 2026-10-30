{
  description = "colony 2: drive herdr and tmux panes, with a locked Telegram bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = [ toolchain ];
      };

      packages.${system}.default = rustPlatform.buildRustPackage {
        pname = "colony";
        version = "1.4.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        # reqwest's `rustls` feature uses the aws-lc-rs crypto provider, whose
        # aws-lc-sys crate compiles C code: it needs cmake and perl (and the
        # C compiler stdenv already provides). No OpenSSL, so no pkg-config.
        nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
      };
    };
}
