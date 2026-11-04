#!/bin/bash
# Builds plume_<version>_<arch>.deb with dpkg-deb (no extra tooling needed).
# Run on Ubuntu 24.04 or newer; the package installs on 24.04 and 26.04.
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(dpkg --print-architecture)
UUID=plume-floats@feederbrian.github.io
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

cargo build --release -p plume
install -Dm755 target/release/plume                      "$STAGE/usr/bin/plume"
install -Dm644 data/io.github.feederbrian.Plume.desktop        "$STAGE/usr/share/applications/io.github.feederbrian.Plume.desktop"
install -Dm644 data/io.github.feederbrian.Plume.metainfo.xml   "$STAGE/usr/share/metainfo/io.github.feederbrian.Plume.metainfo.xml"
install -Dm644 data/icons/io.github.feederbrian.Plume.svg      "$STAGE/usr/share/icons/hicolor/scalable/apps/io.github.feederbrian.Plume.svg"
install -Dm644 extension/$UUID/metadata.json             "$STAGE/usr/share/gnome-shell/extensions/$UUID/metadata.json"
install -Dm644 extension/$UUID/extension.js              "$STAGE/usr/share/gnome-shell/extensions/$UUID/extension.js"
install -Dm644 LICENSE                                   "$STAGE/usr/share/doc/plume/copyright"

# Shared-library dependencies, computed the Debian way.
WORK=$(mktemp -d); mkdir -p "$WORK/debian"
printf 'Source: plume\n\nPackage: plume\nArchitecture: any\n' > "$WORK/debian/control"
DEPS=$(cd "$WORK" && dpkg-shlibdeps -O "$STAGE/usr/bin/plume" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
rm -rf "$WORK"

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<CTL
Package: plume
Version: $VERSION
Architecture: $ARCH
Maintainer: Brian Soria <brian.m.soria@gmail.com>
Depends: $DEPS
Recommends: policykit-1 | pkexec, gnome-shell-extension-prefs
Suggests: pciutils
Section: admin
Priority: optional
Homepage: https://github.com/feederbrian/plume
Description: featherweight system monitor with live 60 fps graphs
 Per-process CPU, memory, disk and network tabs, Intel/NVIDIA/AMD GPU
 monitoring that lets a sleeping laptop GPU stay asleep, floating
 always-on-top readouts (with the bundled GNOME Shell extension), and
 themes including Tokyo Night, Solarized, Nord, Gruvbox and Catppuccin.
CTL
cat > "$STAGE/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
exit 0
POST
chmod 755 "$STAGE/DEBIAN/postinst"
OUT="plume_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT" >/dev/null
echo "$OUT"
dpkg-deb --info "$OUT" | sed -n '/Depends/p'
