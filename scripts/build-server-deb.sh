#!/usr/bin/env bash
# Builds netagg-server_<version>_amd64.deb: NetAgg for headless Linux servers
# (the service and its WebUI, without the tray app, GTK or WebKit). Run on
# the oldest Ubuntu it should install on (24.04), so glibc matches:
#
#   scripts/build-server-deb.sh
#
# It also builds netagg-apt_<version>_all.deb, which only adds NetAgg's apt
# repository and its key: installing it is how a server opts in to updates
# through apt, so netagg-server itself never touches /etc/apt.
#
# Both land in src-tauri/target/release/bundle/deb/, and with them apt/: the
# packages plus their Packages index, the unsigned half of NetAgg's apt
# repository (scripts/release.sh --linux-from signs it).
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION="$(node -p 'require("./src-tauri/tauri.conf.json").version')"
ARCH=amd64
OUT="src-tauri/target/release/bundle/deb"
ROOT="$(mktemp -d)"
APT_ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT" "$APT_ROOT"' EXIT

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

# netagg-apt: NetAgg's apt source and the key it's signed with, nothing else.
# Configuration files: a removal keeps the two together, a purge removes both.
install -Dm644 src-tauri/linux/netagg.sources "$APT_ROOT/etc/apt/sources.list.d/netagg.sources"
install -Dm644 src-tauri/linux/netagg-archive-keyring.gpg "$APT_ROOT/etc/apt/keyrings/netagg-archive-keyring.gpg"
install -Dm644 LICENSE "$APT_ROOT/usr/share/doc/netagg-apt/copyright"
chmod -R u=rwX,go=rX "$APT_ROOT/etc" "$APT_ROOT/usr"
install -d "$APT_ROOT/DEBIAN"
FINGERPRINT="$(gpg --show-keys --with-colons src-tauri/linux/netagg-archive-keyring.gpg | awk -F: '/^fpr/ {print $10; exit}')"
cat > "$APT_ROOT/DEBIAN/control" <<EOF
Package: netagg-apt
Version: $VERSION
Architecture: all
Maintainer: shogorik <46198806+shogorik@users.noreply.github.com>
Installed-Size: $(du -sk "$APT_ROOT/etc" "$APT_ROOT/usr" | awk '{s += $1} END {print s}')
Section: admin
Priority: optional
Homepage: https://github.com/shogorik/NetAgg
Description: NetAgg's apt repository, for updates with apt upgrade
 Adds https://shogorik.github.io/NetAgg/apt/ to apt, trusted only with
 NetAgg's repository key (fingerprint $FINGERPRINT),
 so new netagg-server versions come with "sudo apt update && sudo apt
 upgrade". Remove it to stop; purge it to remove the source and the key.
EOF
printf '%s\n' /etc/apt/sources.list.d/netagg.sources /etc/apt/keyrings/netagg-archive-keyring.gpg \
  > "$APT_ROOT/DEBIAN/conffiles"
APT_DEB="$OUT/netagg-apt_${VERSION}_all.deb"
dpkg-deb --root-owner-group --build "$APT_ROOT" "$APT_DEB"
echo "$APT_DEB"

# The repository's package index (flat: no dists/ tree), for release.sh to sign.
# netagg-apt is in it too, so a new key or address can reach servers through it.
rm -rf "$OUT/apt"
mkdir -p "$OUT/apt"
cp "$DEB" "$APT_DEB" "$OUT/apt/"
(cd "$OUT/apt" && dpkg-scanpackages --multiversion . /dev/null > Packages 2>/dev/null && gzip -9kn Packages)
echo "$OUT/apt"
