{
  lib,
  rustPlatform,
  makeWrapper,
  zenity,
  withPicker ? true,
}:

rustPlatform.buildRustPackage {
  pname = "niri-screenshare";
  version = "0.1.0";

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

  nativeBuildInputs = lib.optionals withPicker [ makeWrapper ];

  postInstall = ''
    mkdir -p $out/lib $out/bin $out/share/dbus-1/services $out/lib/systemd/user

    mv $out/bin/niri-screenshare $out/lib/niri-screenshare
    ln -sf $out/lib/niri-screenshare $out/bin/niri-screenshare

    install -Dm644 data/niri.portal \
      $out/share/xdg-desktop-portal/portals/niri.portal

    install -Dm644 data/niri-portals.conf \
      $out/share/xdg-desktop-portal/niri-portals.conf

    substitute data/org.freedesktop.impl.portal.desktop.niri.service \
      $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.niri.service \
      --replace-fail /usr/lib/niri-screenshare "$out/lib/niri-screenshare"

    substitute data/niri-screenshare.service \
      $out/lib/systemd/user/niri-screenshare.service \
      --replace-fail /usr/lib/niri-screenshare "$out/lib/niri-screenshare"
  ''
  + lib.optionalString withPicker ''
    wrapProgram $out/lib/niri-screenshare \
      --prefix PATH : ${lib.makeBinPath [ zenity ]}
  '';

  meta = {
    description = "Portal backend for niri implementing ScreenCast";
    homepage = "https://github.com/pantarune/niri-screenshare/";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux;
    mainProgram = "niri-screenshare";
  };
}
