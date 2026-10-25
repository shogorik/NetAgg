#!/usr/bin/env bash
# Builds netagg-server_<version>_amd64.deb: NetAgg for headless Linux servers
# (the service and its WebUI, without the tray app, GTK or WebKit). Run on
# the oldest Ubuntu it should install on (24.04), so glibc matches:
#
#   scripts/build-server-deb.sh
#
# The package lands in src-tauri/target/release/bundle/deb/.
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION="$(node -p 'require("./src-tauri/tauri.conf.json").version')"
ARCH=amd64
OUT="src-tauri/target/release/bundle/deb"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

npm run build
(cd src-tauri && cargo build --release --no-default-features --features server --bin netagg-server)

install -Dm755 src-tauri/target/release/netagg-server "$ROOT/usr/bin/netagg-server"
mkdir -p "$ROOT/usr/share/netagg-server/www"
cp -r dist/. "$ROOT/usr/share/netagg-server/www/"
# The same service as the desktop package, run from netagg-server.
install -d "$ROOT/usr/lib/systemd/system"
sed 's#/usr/bin/netagg #/usr/bin/netagg-server #g' src-tauri/linux/netagg.service \
  > "$ROOT/usr/lib/systemd/system/netagg.service"
grep -q '^ExecStart=/usr/bin/netagg-server --service$' "$ROOT/usr/lib/systemd/system/netagg.service"
install -m644 src-tauri/linux/netagg-webui.service "$ROOT/usr/lib/systemd/system/"
install -Dm644 LICENSE "$ROOT/usr/share/doc/netagg-server/copyright"

# Nothing but root may change what root runs, whatever the build user's umask.
chmod -R u=rwX,go=rX "$ROOT/usr"

GLIBC="$(objdump -T "$ROOT/usr/bin/netagg-server" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 | cut -d_ -f2)"
SIZE="$(du -sk "$ROOT/usr" | cut -f1)"
install -d "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<EOF
Package: netagg-server
Version: $VERSION
Architecture: $ARCH
Maintainer: shogorik <46198806+shogorik@users.noreply.github.com>
Installed-Size: $SIZE
Depends: libc6 (>= $GLIBC), nftables, iproute2, adduser
Conflicts: net-agg
Section: net
Priority: optional
Homepage: https://github.com/shogorik/NetAgg
Description: NetAgg for headless servers: combine several networks
 Spreads the whole machine's connections across Wi-Fi, Ethernet and a
 USB-tethered phone at once. This package runs NetAgg as a service with a
 WebUI on port 8443 (HTTPS, password-protected, local networks only);
 set its password with: sudo netagg-server webui-password
EOF

cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    if ! getent passwd netagg-web >/dev/null; then
        adduser --system --group --no-create-home --home /nonexistent netagg-web
    fi
    if [ -d /run/systemd/system ]; then
        systemctl daemon-reload
        systemctl enable netagg.service netagg-webui.service
        systemctl restart netagg.service netagg-webui.service
    fi
    if [ ! -s /etc/netagg/webui-password ]; then
        echo "NetAgg: set the WebUI password with: sudo netagg-server webui-password"
    fi
fi
EOF
cat > "$ROOT/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then
    case "$1" in
        remove|deconfigure) systemctl disable --now netagg-webui.service netagg.service || true ;;
        upgrade) systemctl stop netagg-webui.service netagg.service || true ;;
    esac
fi
EOF
cat > "$ROOT/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = purge ]; then
    rm -rf /var/lib/netagg /var/lib/netagg-web /etc/netagg
    if getent passwd netagg-web >/dev/null; then
        deluser --system netagg-web >/dev/null || true
    fi
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
EOF
chmod 755 "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/prerm" "$ROOT/DEBIAN/postrm"

mkdir -p "$OUT"
DEB="$OUT/netagg-server_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$DEB"
echo "$DEB"
