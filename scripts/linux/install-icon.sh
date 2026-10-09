#!/usr/bin/env bash
# 为便携版 lterm 注册任务栏/启动器图标
# 用法: ./install-icon.sh [lterm 二进制路径]
#   不带参数时自动在同目录里找 lterm / lterm-linux-x64-portable
# 说明: Icon= 用绝对路径，不依赖图标主题查找（Cinnamon 找不到主题图标时会把
#       StartupWMClass 匹配到的任务栏按钮渲染成无图标空壳）；同时仍写入 hicolor，
#       供按名查找的场景兜底。
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-}"
if [ -z "$BIN" ]; then
  for c in "$SCRIPT_DIR/lterm" "$SCRIPT_DIR/lterm-linux-x64-portable"; do
    [ -x "$c" ] && BIN="$c" && break
  done
fi
[ -n "$BIN" ] || { echo "用法: ./install-icon.sh /path/to/lterm"; exit 1; }
BIN="$(readlink -f "$BIN")"
[ -x "$BIN" ] || { echo "找不到可执行文件: $BIN"; exit 1; }

SRC=""
for c in "$(dirname "$BIN")/icon.png" "$SCRIPT_DIR/icon.png" "$(cd "$SCRIPT_DIR/../../src-tauri/icons" 2>/dev/null && pwd)/icon.png"; do
  [ -f "$c" ] && SRC="$c" && break
done
[ -n "$SRC" ] || { echo "找不到 icon.png（应与 lterm 同目录）"; exit 1; }

ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
ICON_PATH="$ICON_DIR/lterm.png"
mkdir -p "$ICON_DIR"
if command -v convert >/dev/null 2>&1; then
  convert -resize 256x256 "$SRC" "$ICON_PATH"
else
  cp "$SRC" "$ICON_PATH"
fi
# hicolor 主题需要 index.theme 声明目录，否则用户目录下的主题可能不被识别
if [ ! -f "$HOME/.local/share/icons/hicolor/index.theme" ]; then
  cat > "$HOME/.local/share/icons/hicolor/index.theme" <<'EOF'
[Icon Theme]
Name=hicolor
Comment=Fallback icon theme
Directories=256x256/apps

[256x256/apps]
Size=256
Context=Applications
Type=Fixed
EOF
fi
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

DESKTOP="$HOME/.local/share/applications/lterm.desktop"
mkdir -p "$HOME/.local/share/applications"
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=lterm
GenericName=SSH Terminal
Comment=Rust SSH 终端
Exec="$BIN"
Icon=$ICON_PATH
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=lterm
EOF
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

echo "已注册: $DESKTOP"
echo "  Exec: $BIN"
echo "  Icon: $ICON_PATH"
echo "若任务栏按钮仍未刷新，注销重登一次（Cinnamon 会缓存启动器图标）"
