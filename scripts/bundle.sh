#!/bin/bash
# Builds dist/Mac Storage Cleaner.app, signs it ad hoc with the hardened runtime, and packs it
# into dist/Mac-Storage-Cleaner-<version>.dmg next to the matching dSYM.
#
# SENTRY_DSN is read from the environment, or from .env if it isn't set. Without it the app is
# built with crash reporting disabled. No other value from .env is used here.
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="Mac Storage Cleaner"
EXECUTABLE="mac-storage-cleaner"
BUNDLE_ID="io.github.07rjain.mac-storage-cleaner"
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
DIST="dist"
DMG="$DIST/Mac-Storage-Cleaner-$VERSION.dmg"
# Built outside the source tree: iCloud-synced folders such as Desktop attach Finder metadata
# that codesign rejects.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
STAGING="$WORK/dmg"
APP="$STAGING/$APP_NAME.app"

if [[ -z "${SENTRY_DSN:-}" && -f .env ]]; then
    SENTRY_DSN=$(sed -n 's/^SENTRY_DSN=//p' .env | head -n 1 | tr -d '"'"'"'')
fi
if [[ -n "${SENTRY_DSN:-}" ]]; then
    export SENTRY_DSN
    echo "Crash reporting: on"
else
    echo "Crash reporting: off (SENTRY_DSN is not set)"
fi

cargo build --release --locked -p app

TARGET_DIR=$(cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
BINARY="$TARGET_DIR/release/$EXECUTABLE"
DSYM="$TARGET_DIR/release/$EXECUTABLE.dSYM"
[[ -d "$DSYM" ]] || { echo "Missing $DSYM; check [profile.release] in Cargo.toml" >&2; exit 1; }

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/$EXECUTABLE"
sed "s/@VERSION@/$VERSION/g" assets/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
cp assets/AppIcon.icns CHANGELOG.md LICENSE THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/"
cp -R licenses "$APP/Contents/Resources/licenses"
xattr -cr "$APP"

# Ad hoc: no Developer ID yet, so Gatekeeper asks the user to confirm the first launch.
codesign --force --options runtime --timestamp=none --identifier "$BUNDLE_ID" --sign - "$APP"
codesign --verify --strict --verbose=1 "$APP"

ln -s /Applications "$STAGING/Applications"
mkdir -p "$DIST"
rm -rf "$DIST/$EXECUTABLE.dSYM" "$DMG"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGING" -fs HFS+ -format UDZO -ov "$DMG"
# The symlink Cargo leaves in target/release is followed so dist/ holds the real files.
cp -RL "$DSYM" "$DIST/$EXECUTABLE.dSYM"

APP_UUID=$(dwarfdump --uuid "$APP/Contents/MacOS/$EXECUTABLE" | awk '{print $2}')
DSYM_UUID=$(dwarfdump --uuid "$DIST/$EXECUTABLE.dSYM" | awk '{print $2}')
[[ "$APP_UUID" == "$DSYM_UUID" ]] || { echo "dSYM $DSYM_UUID doesn't match app $APP_UUID" >&2; exit 1; }
echo "Built $DMG"
echo "Debug symbols: $DIST/$EXECUTABLE.dSYM (UUID $APP_UUID)"
