// 内置图标集：24x24 网格、描边风格（fill=none / stroke=currentColor），
// 不依赖任何图标字体，Windows(WebView2) 与 Linux(WebKitGTK) 渲染结果一致。
// 键名兼容 tabby 的写法：填 fas fa-desktop 会归一化成 desktop。
export const ICONS: Record<string, string> = {
  server: '<rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/>',
  desktop: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M9 20h6M12 16v4"/>',
  laptop: '<rect x="4" y="5" width="16" height="11" rx="2"/><path d="M2 19h20"/>',
  terminal: '<path d="M4 6l6 6-6 6"/><path d="M12 18h8"/>',
  database: '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/>',
  cloud: '<path d="M17.5 18a4.5 4.5 0 0 0 .4-8.98A6 6 0 0 0 6.3 10.1 3.95 3.95 0 0 0 7 18h10.5z"/>',
  code: '<path d="M8 6l-6 6 6 6M16 6l6 6-6 6"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3.6 3 14.4 0 18M12 3c-3 3.6-3 14.4 0 18"/>',
  shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  star: '<path d="M12 3l2.9 5.9 6.5.9-4.7 4.6 1.1 6.5L12 17.8 6.2 20.9l1.1-6.5L2.6 9.8l6.5-.9z"/>',
  mobile: '<rect x="7" y="2" width="10" height="20" rx="2"/><path d="M11 18h2"/>',
  wifi: '<path d="M2 8.5a15 15 0 0 1 20 0M5.5 12.2a10 10 0 0 1 13 0M9 15.8a5 5 0 0 1 6 0"/><path d="M12 19h.01"/>',
  hdd: '<rect x="3" y="6" width="18" height="12" rx="2"/><circle cx="12" cy="12" r="3"/><path d="M12 12h.01"/>',
  box: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>',
  network: '<rect x="9" y="3" width="6" height="6" rx="1"/><rect x="2" y="15" width="6" height="6" rx="1"/><rect x="16" y="15" width="6" height="6" rx="1"/><path d="M12 9v3M5 15v-3h14v3"/>',
  lock: '<rect x="4" y="10" width="16" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>',
  key: '<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l2 2M14 9l2 2"/>',
  cpu: '<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M4 10h3M4 14h3M17 10h3M17 14h3M10 4v3M14 4v3M10 17v3M14 17v3"/>',
  printer: '<path d="M6 9V3h12v6"/><rect x="3" y="9" width="18" height="8" rx="2"/><path d="M6 17h12v4H6z"/>',
  home: '<path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/>',
  mail: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 7l9 6 9-6"/>',
  phone: '<path d="M5 3h4l2 5-3 2a11 11 0 0 0 5 5l2-3 5 2v4a2 2 0 0 1-2 2A17 17 0 0 1 3 5a2 2 0 0 1 2-2z"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  zap: '<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>',
  flag: '<path d="M5 21V4M5 4h13l-2.5 4L18 12H5"/>',
  gamepad: '<rect x="2" y="8" width="20" height="9" rx="4.5"/><path d="M7 11v4M5 13h4M16 12h.01M18.5 14h.01"/>',
  briefcase: '<rect x="3" y="7" width="18" height="13" rx="2"/><path d="M9 7V5a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2M3 12h18"/>',
  bell: '<path d="M6 9a6 6 0 0 1 12 0c0 5 2 6 2 6H4s2-1 2-6"/><path d="M10 19a2 2 0 0 0 4 0"/>',
  camera: '<path d="M4 8h3l2-2h6l2 2h3a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z"/><circle cx="12" cy="13" r="3.5"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>',
};

export const ICON_NAMES = Object.keys(ICONS);

/// `fas fa-desktop` / `fa-desktop` / `desktop` / `fas-desktop` 都归一到 desktop
export function normalizeIcon(raw: string): string {
  const parts = raw.trim().toLowerCase().split(/[\s_-]+/).filter(Boolean);
  while (parts.length && /^(fas|far|fab|fad|fa|icon|img)$/.test(parts[0])) parts.shift();
  return parts.join('-').replace(/^fa-/, '');
}

export function iconPaths(raw: string): string | null {
  return ICONS[normalizeIcon(raw)] ?? null;
}
