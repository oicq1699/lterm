#!/usr/bin/env bash
# 在能访问 GitHub 的服务器上镜像抓取 lterm 发布产物：
#   1. 查询 GitHub 最新 Release，与本地 last_version 记录比较；
#   2. 有新版本则在当前目录创建 <版本号>/（如 0.6.0/）；
#   3. 把该版本全部产物（NSIS 安装器、便携 exe、AppImage、签名、latest.json）下载进去。
#
# 用法:
#   ./fetch-release.sh            正常检查并按需下载
#   ./fetch-release.sh --dry-run  只列出将要下载的内容，不实际下载
#   ./fetch-release.sh --force    忽略版本记录，强制重新下载最新版本
#
# 环境变量:
#   REPO                        仓库，默认 oicq1699/lterm
#   GITHUB_TOKEN                可选，提高 API 限额（匿名 60 次/小时）
#   LOW_SPEED_LIMIT / LOW_SPEED_TIME  低速中断阈值，默认低于 1KB/s 持续 60 秒判定失败并重试
#
# 建议 cron 定时执行，例如每天 8 点:
#   0 8 * * * cd /srv/lterm-mirror && ./fetch-release.sh >> fetch.log 2>&1
#
# 下载失败时不会写 last_version；重新运行会对已完成的文件跳过、
# 对中断的文件用 .part 断点续传。

set -euo pipefail

REPO="${REPO:-oicq1699/lterm}"
STATE_FILE="last_version"
DRY_RUN=0
FORCE=0

usage() {
  sed -n '2,23p' "$0"
}

for arg in "$@"; do
  case "$arg" in
    --dry-run|-n) DRY_RUN=1 ;;
    --force|-f) FORCE=1 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "未知参数: $arg（支持 --dry-run / --force / --help）" >&2
      exit 2
      ;;
  esac
done

command -v curl >/dev/null 2>&1 || {
  echo "缺少 curl，请先安装" >&2
  exit 1
}

api="https://api.github.com/repos/${REPO}/releases/latest"
header=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  header=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi

json=$(curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 "${header[@]}" "$api") || {
  echo "请求 GitHub API 失败: $api" >&2
  exit 1
}

if command -v jq >/dev/null 2>&1; then
  tag=$(printf '%s' "$json" | jq -r '.tag_name')
  urls=$(printf '%s' "$json" | jq -r '.assets[].browser_download_url')
else
  tag=$(printf '%s\n' "$json" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
  urls=$(printf '%s\n' "$json" | sed -n 's/.*"browser_download_url":[[:space:]]*"\([^"]*\)".*/\1/p')
fi

[ -n "$tag" ] || {
  echo "无法从 API 返回中解析 tag_name" >&2
  exit 1
}
[ -n "$urls" ] || {
  echo "$tag 没有任何发布产物" >&2
  exit 1
}

version="${tag#v}"
current=""
[ -f "$STATE_FILE" ] && current=$(cat "$STATE_FILE")

if [ "$FORCE" -eq 0 ] && [ -n "$version" ] && [ "$version" = "$current" ]; then
  echo "已是最新：$tag（记录于 ./$STATE_FILE）"
  exit 0
fi

echo "发现新版本: ${current:-无本地记录} -> $tag"

if [ "$DRY_RUN" -eq 1 ]; then
  echo "（--dry-run）将在 ./$version/ 下载以下产物："
  printf '%s\n' "$urls" | while IFS= read -r u; do
    [ -n "$u" ] && echo "  ${u##*/}"
  done
  exit 0
fi

mkdir -p "$version"

dl() {
  local url="$1" tmp="$2.part"
  local speed=(--speed-limit "${LOW_SPEED_LIMIT:-1024}" --speed-time "${LOW_SPEED_TIME:-60}")
  if ! curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 "${speed[@]}" \
    -C - -o "$tmp" "$url"; then
    rm -f "$tmp"
    curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 "${speed[@]}" \
      -o "$tmp" "$url"
  fi
  mv -f "$tmp" "$2"
}

failed=0
while IFS= read -r u; do
  [ -n "$u" ] || continue
  name="${u##*/}"
  dest="$version/$name"
  if [ -f "$dest" ]; then
    echo "跳过（已存在）: $dest"
    continue
  fi
  echo "下载: $name"
  if dl "$u" "$dest"; then
    echo "  -> $dest"
  else
    echo "  下载失败: $name" >&2
    failed=1
  fi
done <<<"$urls"

if [ "$failed" -ne 0 ]; then
  echo "存在失败的文件，不更新 $STATE_FILE；重新运行可续传/重试" >&2
  exit 1
fi

echo "$version" >"$STATE_FILE"
echo "完成：$tag 已镜像到 ./$version/，版本记录已更新（./$STATE_FILE）"
