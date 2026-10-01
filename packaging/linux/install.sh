#!/bin/sh
# Install Gantry for the current user into ~/.local, or remove it with --uninstall.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
id=io.github.idogrf.Gantry
bin_dir=$HOME/.local/bin
data=${XDG_DATA_HOME:-$HOME/.local/share}
desktop=$data/applications/$id.desktop
icon=$data/icons/hicolor/scalable/apps/$id.svg
case "${1:-}" in
  "")
    install -Dm0755 "$here/gantry" "$bin_dir/gantry"
    mkdir -p "$(dirname "$desktop")"
    # Absolute path: ~/.local/bin is not always on the desktop session's PATH.
    sed "s|^Exec=gantry|Exec=$bin_dir/gantry|" "$here/$id.desktop" > "$desktop"
    install -Dm0644 "$here/$id.svg" "$icon"
    echo "Installed Gantry: $bin_dir/gantry" ;;
  --uninstall)
    rm -f "$bin_dir/gantry" "$desktop" "$icon"
    echo "Removed Gantry" ;;
  *)
    echo "usage: install.sh [--uninstall]" >&2
    exit 2 ;;
esac
# Refresh caches only where they already exist: a user-level cache that nothing else updates would hide icons and
# entries other apps install later. GNOME picks up new files without them.
if [ -f "$data/applications/mimeinfo.cache" ] && command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "$data/applications" || true
fi
if [ -f "$data/icons/hicolor/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t "$data/icons/hicolor" || true
fi
