#!/usr/bin/env bash
# make_spk.sh — build one universal (noarch) Synology package for DSM 7.
# Usage: ./make_spk.sh [build-number]      (default build number: 0001)
#
# The package contains a static binary for x86_64, aarch64 and armv7; postinst
# keeps the one matching the NAS. Output: dist/dsvideo_passthrough-<ver>.spk

set -euo pipefail

cd "$(dirname "$0")"

need() { command -v "$1" >/dev/null || { echo "❌ '$1' not found"; exit 1; }; }
need jq

# GNU tar is needed (--owner/--no-xattrs): "gtar" on macOS, plain "tar" on Linux.
if command -v gtar >/dev/null; then GTAR=gtar; else GTAR=tar; fi
"$GTAR" --version 2>/dev/null | grep -q "GNU tar" || { echo "❌ GNU tar not found (brew install gnu-tar)"; exit 1; }

md5_of() { if command -v md5sum >/dev/null; then md5sum "$1" | cut -d' ' -f1; else md5 -q "$1"; fi; }

PKG_NAME="dsvideo_passthrough"
BIN="dsvideo-passthrough"
BUILD="${1:-0001}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)-$BUILD"
SRC="synology"
STAGE="target/spk"
OUT="dist/${PKG_NAME}-${VERSION}.spk"

# Rust target → directory name used by scripts/common (binary_path).
TARGETS=(
  "x86_64-unknown-linux-musl:x86_64"
  "aarch64-unknown-linux-musl:aarch64"
  "armv7-unknown-linux-musleabihf:armv7"
)

##############################################################################
# 1. Binaries
##############################################################################
./cross_build_on_mac.sh "${TARGETS[@]%%:*}"

##############################################################################
# 2. Stage package contents
##############################################################################
echo "[INFO] Staging $VERSION in $STAGE …"
rm -rf "$STAGE"
mkdir -p "$STAGE/payload/bin" "$(dirname "$OUT")"

for entry in "${TARGETS[@]}"; do
  target="${entry%%:*}" dir="${entry##*:}"
  mkdir -p "$STAGE/payload/bin/$dir"
  install -m 755 "target/$target/release/$BIN" "$STAGE/payload/bin/$dir/$BIN"
done
install -m 644 "$SRC/dsvideo-passthrough.sc" "$STAGE/payload/"

cp -R "$SRC/conf" "$SRC/scripts" "$SRC/WIZARD_UIFILES" "$STAGE/"
cp "$SRC"/PACKAGE_ICON*.PNG "$STAGE/"
for jf in conf/privilege conf/resource WIZARD_UIFILES/install_uifile; do
  jq empty "$STAGE/$jf" || { echo "❌ Invalid JSON: $jf"; exit 1; }
done

TAR=("$GTAR" --owner=0 --group=0 --numeric-owner --no-xattrs --exclude=.DS_Store)
"${TAR[@]}" -czf "$STAGE/package.tgz" -C "$STAGE/payload" .

sed "s/@VERSION@/$VERSION/" "$SRC/INFO.in" > "$STAGE/INFO"
echo "checksum=\"$(md5_of "$STAGE/package.tgz")\"" >> "$STAGE/INFO"

##############################################################################
# 3. Assemble the .spk (plain tar)
##############################################################################
"${TAR[@]}" -cf "$OUT" -C "$STAGE" \
  INFO package.tgz conf scripts WIZARD_UIFILES PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG

echo "✔  Built: $OUT"
"$GTAR" -tvf "$OUT"
