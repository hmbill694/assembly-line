{ pkgs, ... }:

{
  # `rust-toolchain.toml` is the single source of truth for the version and
  # the components, so a contributor without nix, this shell, and CI can never
  # disagree about which compiler built the code. devenv reads the file rather
  # than taking a channel here — the shell has no rustup to honour it itself.
  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
  };

  packages = with pkgs; [
    cargo-audit # `just audit`, CI's advisory Audit job
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
