# Run like this:
#   nix-build /path/to/this/directory
# ... build products will be in ./result

# Cross-compile via
#  https://github.com/mozilla/nixpkgs-mozilla/issues/91#issuecomment-464483970

{ pkgs ? <nixpkgs>, source ? ./., version ? "dev", crossSystem ? null }:

let
  moz_overlay = import (builtins.fetchTarball https://github.com/mozilla/nixpkgs-mozilla/archive/master.tar.gz);
  nixpkgs = import pkgs { overlays = [ moz_overlay ]; inherit crossSystem; };
  targets = [ nixpkgs.stdenv.targetPlatform.config ];
in
  with nixpkgs;
  stdenv.mkDerivation {
    name = "minimq-${version}";
    #src = lib.cleanSource (lib.sourceByRegex source ["target/*"]);

    # build time dependencies targeting the build platform
    depsBuildBuild = [ buildPackages.stdenv.cc ];
    HOST_CC = "cc";

    # build time dependencies targeting the host platform
    nativeBuildInputs = [
      # to use a specific nighly:
      ((buildPackages.buildPackages.rustChannelOf {date="2020-04-08"; channel="nightly";})
         .rust.override { inherit targets; })
      # ejabberd for testing
      ejabberd
    ];
    shellHook = ''
      export RUSTFLAGS="-C linker=$CC"
    '';
    CARGO_BUILD_TARGET = targets;

    inherit version;

    # Set Environment Variables
    #RUST_TEST_THREADS = 1;
    RUST_BACKTRACE = 1;

}
