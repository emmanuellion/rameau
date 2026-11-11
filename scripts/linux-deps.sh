#!/usr/bin/env bash
# System libraries GPUI links against on Linux (Debian/Ubuntu).
# CI runners sometimes lose their Ubuntu mirrors for a few minutes: retry instead of failing.
set -euo pipefail
pkgs=(libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev
      libx11-xcb-dev libxcb1-dev libvulkan1 libzstd-dev desktop-file-utils)
for attempt in 1 2 3 4; do
  # Short network timeouts: a dead mirror fails fast instead of hanging, so the retries get a chance.
  apt_opts=(-o Acquire::Retries=3 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)
  if sudo apt-get "${apt_opts[@]}" update && \
     sudo apt-get "${apt_opts[@]}" install -y --no-install-recommends "${pkgs[@]}"; then
    exit 0
  fi
  echo "apt a échoué (tentative $attempt), nouvel essai dans 30 s…"
  sleep 30
done
exit 1
