{
  mkShell,
  rustc,
  cargo,
  rustfmt,
  clippy,
  rust-analyzer,
  rustPlatform,
  pkg-config,
  gtk4,
  libadwaita,
  nixfmt,
  niri-screenshare,
}:

mkShell {
  inputsFrom = [ niri-screenshare ];

  packages = [
    rustc
    cargo
    rustfmt
    clippy
    rust-analyzer
    pkg-config
    gtk4
    libadwaita
    nixfmt
  ];

  RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
