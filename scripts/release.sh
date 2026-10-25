#!/usr/bin/env bash
# Builds this system's signed release and adds it to the GitHub release assets:
#
#   scripts/release.sh ["release notes shown in the update prompt"]
#   scripts/release.sh --macos-from <dir> ["notes"]
#
# Run it on Windows (Git Bash), Linux and an Apple silicon Mac in turn, carrying release-assets/
# from one machine to the next: each run adds its bundles, their .sig files and its entries in
# latest.json (the feed the app reads from the latest release), and rewrites SHA256SUMS. A run
# for a new version starts the feed over. Uploads nothing: attach every file in release-assets/
# to the release tagged v<version> on shogorik/NetAgg.
#
# Without a Mac, the macOS workflow (.github/workflows/macos.yml) builds the .app and .dmg, unsigned
# for the updater. --macos-from takes its downloaded artifact instead of building, on any system:
#
#   gh workflow run macos.yml && gh run watch
#   gh run download --name macos-bundle --dir macos-bundle
#   scripts/release.sh --macos-from macos-bundle
#
# The updater key is read from NETAGG_UPDATER_KEY (default ~/.config/netagg/updater.key) by
# path; it is never printed, copied or put in the environment as text.
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="shogorik/NetAgg"
KEY="${NETAGG_UPDATER_KEY:-$HOME/.config/netagg/updater.key}"
# Windows Authenticode: the thumbprint of a code-signing certificate in the user's certificate
# store (self-signed is fine for PCs that trust it). Without the file, nothing is Authenticode-signed.
THUMBPRINT_FILE="${NETAGG_CODESIGN_THUMBPRINT:-$HOME/.config/netagg/codesign.thumbprint}"
MACOS_FROM=""
if [[ "${1:-}" == "--macos-from" ]]; then
  MACOS_FROM="${2:?--macos-from needs the artifact directory}"
  shift 2
fi
NOTES="${1:-}"
OUT="release-assets"

die() { echo "release: $*" >&2; exit 1; }

[[ -f "$KEY" ]] || die "no updater key at $KEY"
VERSION="$(node -p 'require("./src-tauri/tauri.conf.json").version')"
CONF_PUBKEY="$(node -p 'require("./src-tauri/tauri.conf.json").plugins.updater.pubkey')"
# The app trusts the key in tauri.conf.json; signing with any other key gives updates it refuses.
if [[ -f "$KEY.pub" && "$(tr -d '\r\n' < "$KEY.pub")" != "$CONF_PUBKEY" ]]; then
  die "$KEY.pub doesn't match plugins.updater.pubkey in tauri.conf.json"
fi
[[ "$(node -p 'require("./package.json").version')" == "$VERSION" ]] || die "package.json version isn't $VERSION"
grep -q "^version = \"$VERSION\"" src-tauri/Cargo.toml || die "Cargo.toml version isn't $VERSION"

# Each system: its build target, and "feed key=bundle file" for every updater bundle it makes.
# The updater looks for {os}-{arch}-{installer} first, then {os}-{arch}.
NATIVE_PATHS=""
case "$(uname -s)" in
  Darwin)
    [[ "$(stat -f %Lp "$KEY")" == "600" ]] || die "$KEY must be mode 0600 (chmod 600 '$KEY')"
    TARGET="aarch64-apple-darwin"
    BUNDLE="src-tauri/target/$TARGET/release/bundle"
    EXTRA=("dmg/NetAgg_${VERSION}_aarch64.dmg")
    FEED=("darwin-aarch64-app=macos/NetAgg.app.tar.gz" "darwin-aarch64=macos/NetAgg.app.tar.gz")
    ;;
  Linux)
    [[ "$(stat -c %a "$KEY")" == "600" ]] || die "$KEY must be mode 0600 (chmod 600 '$KEY')"
    TARGET="x86_64-unknown-linux-gnu"
    BUNDLE="src-tauri/target/$TARGET/release/bundle"
    EXTRA=()
    FEED=(
      "linux-x86_64-deb=deb/NetAgg_${VERSION}_amd64.deb"
      "linux-x86_64-appimage=appimage/NetAgg_${VERSION}_amd64.AppImage"
      "linux-x86_64=appimage/NetAgg_${VERSION}_amd64.AppImage"
    )
    ;;
  MINGW* | MSYS* | CYGWIN*)
    TARGET="x86_64-pc-windows-msvc"
    BUNDLE="src-tauri/target/$TARGET/release/bundle"
    EXTRA=()
    FEED=(
      "windows-x86_64-nsis=nsis/NetAgg_${VERSION}_x64-setup.exe"
      "windows-x86_64=nsis/NetAgg_${VERSION}_x64-setup.exe"
    )
    # tauri is a Windows program: give it Windows paths.
    NATIVE_PATHS=1
    if [[ -f "$THUMBPRINT_FILE" ]]; then
      THUMBPRINT="$(tr -d '\r\n ' < "$THUMBPRINT_FILE")"
      [[ "$THUMBPRINT" =~ ^[0-9A-Fa-f]{40}$ ]] || die "$THUMBPRINT_FILE isn't a certificate thumbprint"
      SIGN_CONFIG=",\"windows\":{\"certificateThumbprint\":\"$THUMBPRINT\",\"digestAlgorithm\":\"sha256\",\"timestampUrl\":\"http://timestamp.digicert.com\"}"
    fi
    KEY="$(cygpath -w "$KEY")"
    ;;
  *) die "unsupported system $(uname -s)" ;;
esac

if [[ -n "$MACOS_FROM" ]]; then
  # The workflow's artifact: the .dmg and NetAgg.app.tar.gz side by side.
  BUNDLE="$MACOS_FROM"
  EXTRA=("NetAgg_${VERSION}_aarch64.dmg")
  FEED=("darwin-aarch64-app=NetAgg.app.tar.gz" "darwin-aarch64=NetAgg.app.tar.gz")
  archive="$BUNDLE/NetAgg.app.tar.gz"
  [[ -f "$archive" ]] || die "missing $archive"
  [[ -n "$NATIVE_PATHS" ]] && archive="$(cygpath -w "$archive")"
  # Signed here, so the key stays on this machine. --app-version puts the version in the trusted
  # comment, as `tauri build` does, which requireSignedVersion checks.
  npx tauri signer sign -f "$KEY" -p "" --app-version "$VERSION" "$archive"
else
  # CI=true skips the DMG's Finder styling AppleScript, which needs Automation access.
  # `tauri build` takes a key file's path in TAURI_SIGNING_PRIVATE_KEY (it ignores _PATH). The
  # .sig files are only made here, so a plain `npm run tauri build` works without the key.
  CI=true TAURI_SIGNING_PRIVATE_KEY="$KEY" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
    npx tauri build --target "$TARGET" --config "{\"bundle\":{\"createUpdaterArtifacts\":true${SIGN_CONFIG:-}}}"
fi

mkdir -p "$OUT"
# A feed for an older version starts over; one for this version keeps the other systems' entries.
if [[ -f "$OUT/latest.json" && "$(node -p "require('./$OUT/latest.json').version")" != "$VERSION" ]]; then
  rm -f "$OUT"/*
fi

ENTRIES=()
for extra in "${EXTRA[@]}"; do
  [[ -f "$BUNDLE/$extra" ]] || die "missing $BUNDLE/$extra"
  cp "$BUNDLE/$extra" "$OUT/"
done
for entry in "${FEED[@]}"; do
  platform="${entry%%=*}"
  file="${entry#*=}"
  [[ -f "$BUNDLE/$file" ]] || die "missing $BUNDLE/$file"
  [[ -f "$BUNDLE/$file.sig" ]] || die "no updater signature for $file"
  # The macOS archive has no version in its name. Renaming doesn't touch the signature: it covers
  # the bytes, not the name.
  name="$(basename "$file")"
  [[ "$name" == NetAgg.app.tar.gz ]] && name="NetAgg_${VERSION}_aarch64.app.tar.gz"
  cp "$BUNDLE/$file" "$OUT/$name"
  cp "$BUNDLE/$file.sig" "$OUT/$name.sig"
  ENTRIES+=("$platform=$name")
done

VERSION="$VERSION" NOTES="$NOTES" BASE="https://github.com/$REPO/releases/download/v$VERSION" \
  OUT="$OUT" node -e '
    const fs = require("fs");
    const { VERSION, NOTES, BASE, OUT } = process.env;
    const path = `${OUT}/latest.json`;
    const feed = fs.existsSync(path)
      ? JSON.parse(fs.readFileSync(path, "utf8"))
      : { version: VERSION, notes: "", pub_date: "", platforms: {} };
    if (NOTES || !feed.notes) feed.notes = NOTES || `NetAgg ${VERSION}`;
    feed.pub_date = new Date().toISOString().replace(/\.\d+Z$/, "Z");
    for (const entry of process.argv.slice(1)) {
      const [platform, name] = entry.split("=");
      feed.platforms[platform] = {
        signature: fs.readFileSync(`${OUT}/${name}.sig`, "utf8").trim(),
        url: `${BASE}/${name}`,
      };
    }
    fs.writeFileSync(path, JSON.stringify(feed, null, 2) + "\n");
  ' "${ENTRIES[@]}"

if command -v sha256sum > /dev/null; then sum=(sha256sum); else sum=(shasum -a 256); fi
(cd "$OUT" && rm -f SHA256SUMS && "${sum[@]}" * > SHA256SUMS)
echo "release: v$VERSION assets in $OUT/:"
ls -l "$OUT"
