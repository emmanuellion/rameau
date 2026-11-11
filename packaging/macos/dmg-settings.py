# Layout of the Rameau disk image window, read by dmgbuild from the repository root:
#   dmgbuild -s packaging/macos/dmg-settings.py -D app=dist/Rameau.app Rameau out.dmg
# The background (and its @2x version for Retina screens) comes from dmg-background.swift.
import os.path

app = defines["app"]  # noqa: F821 (provided by dmgbuild)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
background = "packaging/macos/background.png"

# Window: 660×400 of background plus the title bar (32 pt), no toolbar or sidebar,
# large icons over the arrow of the background.
# Position measured from the bottom-left of the screen: high enough to open near the top.
window_rect = ((200, 400), (660, 432))
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
default_view = "icon-view"
icon_size = 128
text_size = 13
icon_locations = {os.path.basename(app): (165, 190), "Applications": (495, 190)}
