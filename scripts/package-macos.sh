#!/usr/bin/env bash
# Rameau.app + .dmg from a compiled binary (universal or single arch).
# usage: scripts/package-macos.sh <binary> <version> <out-dir>
set -euo pipefail
bin=$1 version=$2 out=$3
app="$out/Rameau.app"
rm -rf "$app" && mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/rameau"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

# Every icon size macOS asks for, from the 1024px master.
iconset="$(mktemp -d)/rameau.iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/rameau.icns"

# Ad-hoc signature (no Apple Developer account): enough to run on Apple Silicon,
# users approve the app once in System Settings › Privacy & Security.
codesign --force --deep --sign - "$app"

# Disk image: the app, a shortcut to /Applications and the "drag here" background
# (layout in packaging/macos/dmg-settings.py). dmgbuild writes the window layout itself,
# without driving the Finder, so it works on CI machines too.
venv="$(mktemp -d)/venv"
python3 -m venv "$venv"
"$venv/bin/pip" install --quiet --disable-pip-version-check dmgbuild==1.6.5
dmg="$out/rameau-$version-macos-universal.dmg"
"$venv/bin/dmgbuild" -s packaging/macos/dmg-settings.py -D app="$app" Rameau "$dmg" 2>&1 | grep -v deprecated || true
test -s "$dmg"
echo "$dmg"
