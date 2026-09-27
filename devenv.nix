{ pkgs, lib, ... }:

{
  languages.rust.enable = true;
  # the casita-worker crate (Cloudflare Workers) compiles to wasm, which
  # needs a channel toolchain (the nixpkgs one cannot add targets).
  # x86_64-pc-windows-gnu type-checks the Windows cfg branches locally:
  #   cargo check --target x86_64-pc-windows-gnu --all-targets
  languages.rust.channel = "stable";
  # pin one exact toolchain for local dev and CI (no version matrix).
  languages.rust.version = "1.96.0";
  languages.rust.targets = [
    "wasm32-unknown-unknown"
    "x86_64-pc-windows-gnu"
  ];

  # Astro/Starlight documentation site under docs/. Keep dependency
  # installation explicit in that directory; CI uses npm ci against its lock.
  languages.javascript = {
    enable = true;
    package = pkgs.nodejs_24;
    npm.enable = true;
  };

  # Repository and benchmark tools; wrangler + worker-build build and run the
  # casita-worker crate locally (wrangler dev simulates R2 and D1). The worker
  # tooling is gated to Linux: wrangler does not build on macOS in nixpkgs, and
  # CI never touches the worker, so the macos test job does not need it.
  packages = [
    pkgs.git
    pkgs.lychee
    # One pinned environment owns the end-to-end harness and all comparison
    # binaries, so a publication run needs no nested shell or ad-hoc install.
    pkgs.python3
    pkgs.time
    pkgs.gnutar
    # Declare and resolve deployment credentials outside Casita's repository
    # graph. The application continues to consume the standard AWS environment
    # contract, so both S3 clients receive one SecretSpec resolution.
    pkgs.secretspec
    # The wal3 integration test starts this local S3-compatible server and
    # races independent Casita runners against its conditional manifest PUTs.
    pkgs.rustfs
    # wal3's currently in-tree Chroma dependency graph generates protobuf
    # bindings while compiling `chroma-types`.
    pkgs.protobuf
    pkgs.zstd
    pkgs.restic
    pkgs.borgbackup
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    pkgs.wrangler
    pkgs.worker-build
  ] ++ lib.optionals pkgs.stdenv.isDarwin [
    # Native FSKit needs a modern SDK; the default SDK lacks FSKit.framework.
    pkgs.apple-sdk_26
  ];

  # mingw cc for the Windows cargo check, exposed only through the
  # target-scoped variables: putting the cross cc in `packages` would run its
  # setup hook and point CC/CXX at mingw for host builds too.
  env.CC_x86_64_pc_windows_gnu = "${pkgs.pkgsCross.mingwW64.stdenv.cc}/bin/x86_64-w64-mingw32-gcc";
  env.CXX_x86_64_pc_windows_gnu = "${pkgs.pkgsCross.mingwW64.stdenv.cc}/bin/x86_64-w64-mingw32-g++";
  env.AR_x86_64_pc_windows_gnu = "${pkgs.pkgsCross.mingwW64.stdenv.cc.bintools.bintools}/bin/x86_64-w64-mingw32-ar";

  # clippy runs as a git-hook, on commit and in CI via `devenv test`. devenv
  # points it at the toolchain above and turns offline mode off so it can fetch
  # dependencies; the flags reproduce the old CI job
  # (cargo clippy --all-features --all-targets -- -D warnings). Formatting is
  # not a hook: the CI `fmt` job reformats and commits it back instead.
  git-hooks.hooks.clippy = {
    enable = true;
    settings = {
      allFeatures = true;
      denyWarnings = true;
      extraArgs = "--all-targets";
    };
  };

  # `devenv test` runs the clippy hook above, then this: the all-features suite
  # and the default-feature build (git and cli off) a downstream crate would get.
  enterTest = ''
    secretspec check --provider null --no-prompt \
      --reason "validate Casita secret declarations"
    cargo test --all-features
    cargo test
  '';

  processes.docs.exec = ''
    cd docs && npm run dev
  '';

  scripts.benchmark.exec = ''
    python3 -m benchmarks.cli "$@"
  '';

  scripts.git-scale-benchmark.exec = ''
    python3 -m benchmarks.suites.git "$@"
  '';

  scripts.benchmark-dashboard.exec = ''
    python3 -m benchmarks.dashboard "$@"
  '';
}
