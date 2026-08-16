{ pkgs, ... }:

{
  # `rust-toolchain.toml` stays the single source of truth for the version, so
  # a contributor without nix, this shell, and CI can never disagree about
  # which compiler built the code.
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      # rust-analyzer is part of the environment rather than a per-machine
      # install, because it must match the pinned toolchain — a mismatched
      # server crashes on startup rather than degrading.
      "rust-analyzer"
      "rust-src"
    ];
  };

  packages = with pkgs; [
    git
    jujutsu # the repo is organised as a jj stack; see CLAUDE.md
    just
  ];

  enterShell = ''
    echo "assembly-line: $(rustc --version), $(jj --version)"
    echo "run 'just' to see available tasks"
  '';

  # `devenv test` is the same gate as CI and as the per-change stack check.
  enterTest = ''
    just check
  '';
}
