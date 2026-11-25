#!/usr/bin/env bash
# 在能访问 GitHub 的服务器上镜像抓取 lterm 发布产物：
#   1. 查询 GitHub 最新 Release，与本地 last_version 记录比较；
#   2. 有新版本则在当前目录创建 <版本号>/（如 0.6.0/）；
#   3. 把该版本全部产物（NSIS 安装器、便携 exe、AppImage、签名、latest.json）下载进去；
#   4. 把清单里的 GitHub 下载地址改写为本站地址，原子刷新根 latest.json（应用的 updater 指这里）。
#
# 用法:
#   ./fetch-release.sh            正常检查并按需下载
#   ./fetch-release.sh --dry-run  只列出将要下载的内容，不实际下载
#   ./fetch-release.sh --force    忽略版本记录，重新检查最新版并补齐缺失文件（已存在且大小一致的仍跳过）
#
# 环境变量:
#   REPO                        仓库，默认 oicq1699/lterm
#   PUBLISH_BASE                本站对外地址，默认 https://www.linyong.tech/download/lterm
#   GITHUB_TOKEN                可选，提高 API 限额（匿名 60 次/小时）
#   LOW_SPEED_LIMIT / LOW_SPEED_TIME  低速中断阈值，默认低于 1KB/s 持续 60 秒判定失败并重试
#
# 建议 cron 定时执行，例如每天 8 点:
#   0 8 * * * cd /srv/lterm-mirror && ./fetch-release.sh >> fetch.log 2>&1
#
# 下载失败时不会写 last_version；重新运行会跳过已完成文件、
# 用 .part 断点续传中断的文件，并对已存在但大小与发布不符的文件重新下载。

set -euo pipefail

# 每条日志带本地时间，便于从 fetch.log 判断某次运行/某个文件的下载时刻
log() { printf '%s %s\n' "$(date '+%F %T')" "$*"; }
err() { printf '%s %s\n' "$(date '+%F %T')" "$*" >&2; }

REPO="${REPO:-oicq1699/lterm}"
PUBLISH_BASE="${PUBLISH_BASE:-https://www.linyong.tech/download/lterm}"
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
      err "未知参数: $arg（支持 --dry-run / --force / --help）"
      exit 2
      ;;
  esac
done

command -v curl >/dev/null 2>&1 || {
  err "缺少 curl，请先安装"
  exit 1
}

log "开始执行: REPO=${REPO} PUBLISH_BASE=${PUBLISH_BASE}"

api="https://api.github.com/repos/${REPO}/releases/latest"
header=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  header=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi

json=$(curl -fsSL --connect-timeout 15 --retry 3 --retry-delay 5 "${header[@]}" "$api") || {
  err "请求 GitHub API 失败: $api"
  exit 1
}

if command -v jq >/dev/null 2>&1; then
  tag=$(printf '%s' "$json" | jq -r '.tag_name')
  entries=$(printf '%s' "$json" | jq -r '.assets[] | [.size, .browser_download_url] | @tsv')
else
  tag=$(printf '%s\n' "$json" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
  # 无 jq：python3 按 JSON 语义逐行输出 size<TAB>url；没有 python3 时退回 sed（跳过大小校验）
  if command -v python3 >/dev/null 2>&1; then
    entries=$(printf '%s' "$json" | python3 -c "import json,sys
for a in json.load(sys.stdin).get('assets', []):
    print(a['size'], a['browser_download_url'], sep='\t')")
  else
    sizes=$(printf '%s\n' "$json" | sed -n 's/.*"size":[[:space:]]*\([0-9]\+\).*/\1/p')
    urls=$(printf '%s\n' "$json" | sed -n 's/.*"browser_download_url":[[:space:]]*"\([^"]*\)".*/\1/p')
    entries=$(paste <(printf '%s\n' "$sizes") <(printf '%s\n' "$urls"))
  fi
fi

[ -n "$tag" ] || {
  err "无法从 API 返回中解析 tag_name"
  exit 1
}
[ -n "$entries" ] || {
  err "$tag 没有任何发布产物"
  exit 1
}

version="${tag#v}"
current=""
[ -f "$STATE_FILE" ] && current=$(cat "$STATE_FILE")

# 应用端 updater 只认固定地址的 latest.json：把 GitHub 前缀换成本站，先写临时文件再原子替换；
# 改写后仍残留 GitHub 地址则拒绝发布，避免用户"检测到新版却从 GitHub 下载"卡死。
publish_manifest() {
  local src="$version/latest.json" tmp="latest.json.new"
  local oldp="https://github.com/${REPO}/releases/download/${tag}/"
  local newp="${PUBLISH_BASE}/${version}/"
  if [ ! -f "$src" ]; then
    err "缺少 $src，无法发布根 latest.json"
    return 1
  fi
  sed "s|${oldp}|${newp}|g" "$src" >"$tmp"
  if grep -qF "$oldp" "$tmp"; then
    rm -f "$tmp"
    err "清单里仍有未改写的 GitHub 地址，放弃发布根 latest.json"
    return 1
  fi
  if [ -f latest.json ] && cmp -s "$tmp" latest.json; then
    rm -f "$tmp"
    log "根清单无需变化: ./latest.json"
  else
    mv -f "$tmp" latest.json
    log "根清单已发布: ./latest.json（下载地址 → ${newp}）"
  fi
}

if [ "$FORCE" -eq 0 ] && [ -n "$version" ] && [ "$version" = "$current" ]; then
  log "已是最新：$tag（记录于 ./$STATE_FILE）"
  publish_manifest || exit 1
  exit 0
fi

log "发现新版本: ${current:-无本地记录} -> $tag"

if [ "$DRY_RUN" -eq 1 ]; then
  log "（--dry-run）将在 ./$version/ 下载以下产物："
  while IFS=$'\t' read -r _ u; do
    [ -n "$u" ] && log "  ${u##*/}"
  done <<<"$entries"
  log "（--dry-run）并将刷新根 ./latest.json（下载地址 → ${PUBLISH_BASE}/${version}/）"
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

is_num() { case "$1" in ('' | *[!0-9]*) return 1 ;; (*) return 0 ;; esac; }
file_size() { stat -c%s "$1" 2>/dev/null || wc -c <"$1"; }

failed=0
while IFS=$'\t' read -r size u; do
  [ -n "$u" ] || continue
  name="${u##*/}"
  dest="$version/$name"
  if [ -f "$dest" ]; then
    if is_num "$size" && [ "$(file_size "$dest")" != "$size" ]; then
      log "本地大小与发布不符，重新下载: $name"
      rm -f "$dest" "$dest.part"
    else
      log "跳过（已存在且大小一致）: $dest"
      continue
    fi
  elif [ -f "$dest.part" ] && is_num "$size" &&
    [ "$(file_size "$dest.part")" -gt "$size" ]; then
    # 中断残留比发布体积还大说明是脏数据，续传只会拼出坏文件
    rm -f "$dest.part"
  fi
  log "下载: $name"
  if dl "$u" "$dest"; then
    log "  -> $dest"
  else
    err "  下载失败: $name"
    failed=1
  fi
done <<<"$entries"

if [ "$failed" -ne 0 ]; then
  err "存在失败的文件，不更新 $STATE_FILE；重新运行可续传/重试"
  exit 1
fi

publish_manifest || {
  err "根清单发布失败，不更新 $STATE_FILE"
  exit 1
}

echo "$version" >"$STATE_FILE"
log "完成：$tag 已镜像到 ./$version/，版本记录已更新（./$STATE_FILE）"
