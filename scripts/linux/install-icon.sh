#!/usr/bin/env bash
# 为便携版 lterm 注册任务栏/启动器图标（GTK 从图标主题按 Icon= 名查找，不支持任意路径）
# 用法: ./install-icon.sh /绝对路径/到/lterm   （不带参数则假定与脚本同目录的 ./lterm）
set -e
BIN="${1:-$PWD/lterm}"
BIN="$(readlink -f "$BIN")"
BIN_DIR="$(dirname "$BIN")"
[ -x "$BIN" ] || { echo "找不到可执行文件: $BIN"; exit 1; }
SRC=""
for c in "$BIN_DIR/icon.png" "$(cd "$(dirname "$0")/../../src-tauri/icons" 2>/dev/null && pwd)/icon.png"; do
  [ -f "$c" ] && SRC="$c" && break
done
[ -n "$SRC" ] || { echo "找不到 icon.png（应与 lterm 同目录，或仓库 src-tauri/icons/ 下）"; exit 1; }

mkdir -p "$HOME/.local/share/icons/hicolor/256x256/apps"
convert -resize 256x256 "$SRC" "$HOME/.local/share/icons/hicolor/256x256/apps/lterm.png" 2>/dev/null \
  || cp "$SRC" "$HOME/.local/share/icons/hicolor/256x256/apps/lterm.png"
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

mkdir -p "$HOME/.local/share/applications"
cat > "$HOME/.local/share/applications/lterm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=lterm
Comment=Rust SSH 终端
Exec="$BIN"
Icon=lterm
Terminal=false
Categories=Utility;TerminalEmulator;
StartupWMClass=lterm
EOF
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "已注册: $HOME/.local/share/applications/lterm.desktop （图标名 lterm，启动器/任务栏可用）"
