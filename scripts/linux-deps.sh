#!/usr/bin/env bash
# System libraries GPUI links against on Linux (Debian/Ubuntu).
# CI runners sometimes lose their Ubuntu mirrors for a few minutes: retry instead of failing.
set -euo pipefail
pkgs=(libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev
      libx11-xcb-dev libxcb1-dev libvulkan1 libzstd-dev desktop-file-utils)
for attempt in 1 2 3 4; do
  if sudo apt-get -o Acquire::Retries=5 update && \
     sudo apt-get -o Acquire::Retries=5 install -y --no-install-recommends "${pkgs[@]}"; then
    exit 0
  fi
  echo "apt a échoué (tentative $attempt), nouvel essai dans 30 s…"
  sleep 30
done
exit 1
