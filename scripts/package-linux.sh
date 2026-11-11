#!/usr/bin/env bash
# AppImage + .tar.gz from a compiled binary.
# usage: scripts/package-linux.sh <binary> <version> <arch: x86_64|aarch64> <out-dir>
set -euo pipefail
bin=$1 version=$2 arch=$3 out=$4
name="rameau-$version-linux-$arch"
mkdir -p "$out"

# Plain archive: binary, desktop entry and icon for a manual install.
stage="$out/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp "$bin" "$stage/rameau"
cp packaging/linux/rameau.desktop LICENSE README.md "$stage/"
cp assets/icon.png "$stage/rameau.png"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
rm -rf "$stage"

# AppImage: one file to download, chmod +x and run.
appdir="$out/AppDir"
rm -rf "$appdir" && mkdir -p "$appdir/usr/bin"
cp "$bin" "$appdir/usr/bin/rameau"
cp packaging/linux/rameau.desktop "$appdir/"
cp assets/icon.png "$appdir/rameau.png"
ln -s usr/bin/rameau "$appdir/AppRun"
# Pinned version, checked before running it: a changed or tampered tool can't slip into the package.
tool_version=1.9.1
case "$arch" in
  x86_64) tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
  aarch64) tool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
  *) echo "architecture inconnue : $arch" >&2; exit 1 ;;
esac
tool="$out/appimagetool-$arch.AppImage"
curl -sSfL -o "$tool" "https://github.com/AppImage/appimagetool/releases/download/$tool_version/appimagetool-$arch.AppImage"
echo "$tool_sha  $tool" | sha256sum -c -
chmod +x "$tool"
# The CI has no FUSE: let appimagetool unpack itself instead of mounting.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" "$appdir" "$out/$name.AppImage"
rm -rf "$appdir" "$tool"
ls -1 "$out"
