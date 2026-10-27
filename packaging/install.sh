#!/usr/bin/env bash
# Install the AppImage for the current user: binary, app-launcher entry and icon. No root needed.
#   ./packaging/install.sh [path/to/Chat_x.y.z_amd64.AppImage]     install / update
#   ./packaging/install.sh --uninstall                             remove (your chats are NOT touched)
set -euo pipefail
HERE="$(cd "$(dirname "$(readlink -f "$0")")" && pwd)"
ROOT="$(dirname "$HERE")"
BIN_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/chat-app"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps"
TARGET="$BIN_DIR/Chat.AppImage"

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$TARGET" "$APPS_DIR/chat.desktop" "$ICON_DIR/dev.bingus.chat.png"
  rmdir "$BIN_DIR" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" 2>/dev/null || true
  echo "Removed. Your chats in ${XDG_DATA_HOME:-$HOME/.local/share}/dev.bingus.chat/ were left alone."
  exit 0
fi

SRC="${1:-$(ls -t "$ROOT"/src-tauri/target/release/bundle/appimage/*.AppImage 2>/dev/null | head -1)}"
[ -f "$SRC" ] || { echo "No AppImage found. Build one first:  npm run tauri build -- --bundles appimage"; exit 1; }

mkdir -p "$BIN_DIR" "$APPS_DIR" "$ICON_DIR"
install -m 755 "$SRC" "$TARGET"
install -m 644 "$ROOT/src-tauri/icons/icon.png" "$ICON_DIR/dev.bingus.chat.png"
# Edit the Exec line here to add env vars if you ever need them, e.g.:
#   Exec=env WEBKIT_DISABLE_COMPOSITING_MODE=1 /path/to/Chat.AppImage
sed "s|@EXEC@|$TARGET|" "$HERE/chat.desktop" > "$APPS_DIR/chat.desktop"
chmod 644 "$APPS_DIR/chat.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" 2>/dev/null || true
echo "Installed: $TARGET"
echo "Launcher:  $APPS_DIR/chat.desktop   (search for \"Chat\" in your app launcher)"
