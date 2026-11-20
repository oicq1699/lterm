#!/usr/bin/env bash
# 给 Nemo 装「在 lterm 终端中打开」右键项（方案：.nemo_action + 绝对路径 Exec）。
# 用法：scripts/install_nemo_action.sh [lterm二进制路径]
# 卸载：删掉 ~/.local/share/nemo/actions/open-in-lterm*.nemo_action 与 ~/.local/bin/lterm
set -euo pipefail

BIN="${1:-}"
if [[ -z "$BIN" ]]; then
  # 依次找：PATH → 仓库 release 产物
  if command -v lterm >/dev/null 2>&1; then
    BIN="$(command -v lterm)"
  else
    cand="$(cd "$(dirname "$0")/.." && pwd)/src-tauri/target/release/lterm"
    [[ -x "$cand" ]] && BIN="$cand"
  fi
fi
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "找不到 lterm 可执行文件，先 npm run tauri build -- --no-bundle，或把路径作为第一个参数传入" >&2
  exit 1
fi
BIN="$(readlink -f "$BIN")"

# ~/.local/bin 链接一份，终端里手敲 lterm /p 也能用（需该目录在 PATH）
mkdir -p "$HOME/.local/bin"
ln -sf "$BIN" "$HOME/.local/bin/lterm"

ACT="$HOME/.local/share/nemo/actions"
mkdir -p "$ACT"

# 空白处右键：在"当前目录"开终端
cat > "$ACT/open-in-lterm.nemo_action" <<EOF
[Nemo Action]
Active=true
Name=在 lterm 终端中打开
Comment=在当前目录打开一个 lterm 本地终端
Exec="$BIN" "%P"
Icon-Name=utilities-terminal
Selection=none
Extensions=nodirs;
Dependencies=$BIN;
EOF

# 右键选中文件夹：在"那个文件夹"开终端
cat > "$ACT/open-dir-in-lterm.nemo_action" <<EOF
[Nemo Action]
Active=true
Name=在 lterm 终端中打开
Comment=在该文件夹打开一个 lterm 本地终端
Exec="$BIN" "%F"
Icon-Name=utilities-terminal
Selection=s
Extensions=dir;
Dependencies=$BIN;
EOF

echo "已安装到 $ACT"
echo "  二进制: $BIN"
echo "Nemo 一般即时生效；菜单没出现就 nemo --quit && nemo &（或注销重登）"
