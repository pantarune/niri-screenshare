{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  gtk4,
  libadwaita,
  withPicker ? true,
}:

rustPlatform.buildRustPackage {
  pname = "niri-screenshare";
  version = "0.2.2";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../data
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  buildFeatures = lib.optional withPicker "picker";

  nativeBuildInputs = lib.optionals withPicker [
    pkg-config
    wrapGAppsHook4
  ];

  buildInputs = lib.optionals withPicker [
    gtk4
    libadwaita
  ];

  # Start the GApps-wrapped $out/bin binary (schemas via XDG_DATA_DIRS).
  # Do not install our own niri-portals.conf: niri already supplies the
  # desktop-wide defaults, while this backend only owns ScreenCast.
  postInstall = ''
    mkdir -p $out/share/dbus-1/services $out/lib/systemd/user

    install -Dm644 data/niri.portal \
      $out/share/xdg-desktop-portal/portals/niri.portal

    substitute data/org.freedesktop.impl.portal.desktop.niri.service \
      $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.niri.service \
      --replace-fail /usr/lib/niri-screenshare "$out/bin/niri-screenshare"

    substitute data/niri-screenshare.service \
      $out/lib/systemd/user/niri-screenshare.service \
      --replace-fail /usr/lib/niri-screenshare "$out/bin/niri-screenshare"
  '';

  meta = {
    description = "Portal backend for niri implementing ScreenCast";
    homepage = "https://github.com/pantarune/niri-screenshare/";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "niri-screenshare";
  };
}
