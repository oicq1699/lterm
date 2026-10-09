#!/usr/bin/env bash
# 撤销 install-icon.sh 的注册（删除启动器与图标，不动 lterm 本体）
set -e
rm -f "$HOME/.local/share/applications/lterm.desktop"
rm -f "$HOME/.local/share/icons/hicolor/256x256/apps/lterm.png"
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "已移除 lterm 启动器与图标；任务栏若仍异常，注销重登一次即可恢复"
