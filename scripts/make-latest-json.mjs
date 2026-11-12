// 由 CI 调用：把签名后的安装包汇总成 Tauri updater 的 latest.json。
// 用法：node scripts/make-latest-json.mjs <version> <baseUrl> <dir>
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [version, baseUrl, dir] = process.argv.slice(2);
if (!version || !baseUrl || !dir) {
  console.error('用法: node scripts/make-latest-json.mjs <version> <baseUrl> <dir>');
  process.exit(1);
}

const files = readdirSync(dir);
const findAsset = (pattern) => files.find((name) => pattern.test(name));

const platforms = {};
const addPlatform = (key, asset) => {
  const signature = readFileSync(join(dir, `${asset}.sig`), 'utf8').trim();
  platforms[key] = {
    signature,
    url: `${baseUrl}/${encodeURIComponent(asset)}`,
  };
};

// 同一次 Release 里，NSIS 和 AppImage 各自作为两个平台键，方便按包类型分发。
const nsis = findAsset(/-setup\.exe$/);
if (nsis) {
  addPlatform('windows-x86_64', nsis);
  addPlatform('windows-x86_64-nsis', nsis);
}

const appimage = findAsset(/\.AppImage$/);
if (appimage) {
  addPlatform('linux-x86_64', appimage);
  addPlatform('linux-x86_64-appimage', appimage);
}

if (Object.keys(platforms).length === 0) {
  console.error('没有找到 NSIS 或 AppImage 产物，无法生成 latest.json');
  process.exit(1);
}

const manifest = {
  version,
  notes: '',
  pub_date: new Date().toISOString(),
  platforms,
};

writeFileSync(join(dir, 'latest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
