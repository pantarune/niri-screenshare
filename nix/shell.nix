{
  mkShell,
  rustc,
  cargo,
  rustfmt,
  clippy,
  rust-analyzer,
  rustPlatform,
  zenity,
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
    zenity
    nixfmt
  ];

  RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
