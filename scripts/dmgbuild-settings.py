"""dmgbuild settings for the WorkBuddy Switch drag-to-Applications image.

This file is evaluated by ``dmgbuild``.  Keep the image contents deliberately
small: the app bundle and one Applications symlink are the only user-facing
items at the volume root.  ``builtin-arrow`` is rendered by dmgbuild as a
retina-capable, non-branded arrow between those items.
"""

import os


application = defines.get("app")  # noqa: F821 - provided by dmgbuild
if not application:
    raise ValueError("dmgbuild requires -D app=/path/to/workbuddy-switch.app")

application = os.path.abspath(application)
if not os.path.isdir(application):
    raise ValueError(f"application bundle does not exist: {application}")

appname = os.path.basename(application)

# Exactly two actionable root items: the app and the Applications shortcut.
files = [application]
symlinks = {"Applications": "/Applications"}

# Finder icon view layout, in points.  Keep enough space around the built-in
# arrow so it cannot overlap either icon or its label.
default_view = "icon-view"
include_icon_view_settings = True
icon_size = 128
label_pos = "bottom"
text_size = 16
icon_locations = {
    appname: (205, 270),
    "Applications": (555, 270),
}

# Clean Finder presentation without a decorative image background.  The
# built-in arrow is the only visual layer beyond the two filesystem items.
background = "builtin-arrow"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

window_rect = ((100, 100), (760, 525))

# Keep the low-level image format explicit so this remains equivalent to the
# previous hdiutil -format UDZO invocation.
format = "UDZO"
