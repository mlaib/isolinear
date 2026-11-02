#!/bin/bash
# Builds the Quick Look extension, puts it inside Isolinear.app, signs both and makes the .dmg.
# Run on macOS after `npm run tauri build -- --bundles app`. Signs ad hoc unless
# APPLE_SIGNING_IDENTITY names a certificate in the keychain.
set -euo pipefail
cd "$(dirname "$0")/../../.."
HERE=packaging/macos/quicklook
APP=target/release/bundle/macos/Isolinear.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
OUT=target/release/bundle/quicklook
test -d "$APP" || { echo "build the app first: npm run tauri build -- --bundles app" >&2; exit 1; }
rm -rf "$OUT" && mkdir -p "$OUT"

# The preview library, and the system libraries it needs when linked statically.
cargo build --release -p isolinear-preview
LIBS=$(cargo --color never rustc --release -p isolinear-preview --lib -- --print native-static-libs 2>&1 |
  sed -e 's/\x1b\[[0-9;]*m//g' -n -e 's/.*native-static-libs: //p' | tail -1)
echo "linking with: $LIBS"

swiftc -O -module-name IsolinearQuickLook -parse-as-library -application-extension \
  -target "$ARCH-apple-macos12.0" \
  -import-objc-header "$HERE/Bridge.h" "$HERE/PreviewProvider.swift" \
  target/release/libisolinear_preview.a $LIBS \
  -framework QuickLookUI -framework UniformTypeIdentifiers \
  -Xlinker -e -Xlinker _NSExtensionMain \
  -o "$OUT/IsolinearQuickLook"

APPEX="$APP/Contents/PlugIns/IsolinearQuickLook.appex"
rm -rf "$APPEX" && mkdir -p "$APPEX/Contents/MacOS"
cp "$OUT/IsolinearQuickLook" "$APPEX/Contents/MacOS/"
sed "s/@VERSION@/$VERSION/" "$HERE/Info.plist" > "$APPEX/Contents/Info.plist"
plutil -lint "$APPEX/Contents/Info.plist" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Print :UTExportedTypeDeclarations:0:UTTypeIdentifier" "$APP/Contents/Info.plist"

# The extension first, then the app around it (which seals the extension's signature).
ID=${APPLE_SIGNING_IDENTITY:--}
codesign --force --sign "$ID" --options runtime --entitlements "$HERE/entitlements.plist" "$APPEX"
codesign --force --sign "$ID" --options runtime "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
# Informational: whether this Mac lists the extension (it registers when the app is opened).
pluginkit -a "$APPEX" 2>/dev/null || true
pluginkit -m -v -i io.github.mlaib.isolinear.quicklook || true

DMG_DIR=target/release/bundle/dmg
case "$ARCH" in arm64) DMG_ARCH=aarch64 ;; *) DMG_ARCH=$ARCH ;; esac
STAGE="$OUT/dmg"
rm -rf "$STAGE" "$DMG_DIR" && mkdir -p "$STAGE" "$DMG_DIR"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname Isolinear -srcfolder "$STAGE" -ov -format UDZO \
  "$DMG_DIR/Isolinear_${VERSION}_${DMG_ARCH}.dmg"
echo "Quick Look extension inside $APP; disk image in $DMG_DIR"
