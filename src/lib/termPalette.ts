/** 终端 16 色 ANSI 调色板；xterm.js 自带的是 Tango 系，暗背景上绿/黄/蓝偏暗。 */

const ANSI_KEYS = [
  'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite',
] as const;

export const PALETTES = {
  // xterm.js 默认（Tango 系）
  tango: {
    foreground: '#d4d4d4',
    ansi: ['#2e3436', '#cc0000', '#4e9a06', '#c4a000', '#3465a4', '#75507b', '#06989a', '#d3d7cf',
      '#555753', '#ef2929', '#8ae234', '#fce94f', '#729fcf', '#ad7fa8', '#34e2e2', '#eeeeec'],
  },
  // 与真正的 xterm(1) 一致的 XT-16：常规色更饱和更亮，蓝/紫偏深（弱屏上建议配合对比度增强）
  xterm: {
    foreground: '#e5e5e5',
    ansi: ['#000000', '#cd0000', '#00cd00', '#cdcd00', '#0000ee', '#cd00cd', '#00cdcd', '#e5e5e5',
      '#7f7f7f', '#ff0000', '#00ff00', '#ffff00', '#5c5cff', '#ff00ff', '#00ffff', '#ffffff'],
  },
  // 高对比：把暗背景上读不清的蓝/紫/绿整体提亮，黑也抬成灰
  high: {
    foreground: '#f0f6fc',
    ansi: ['#6e7681', '#ff7b72', '#7ee787', '#e3b341', '#79c0ff', '#d2a8ff', '#56d4dd', '#c9d1d9',
      '#8b949e', '#ffa198', '#aff5b4', '#f2cc60', '#a5d6ff', '#e2c5ff', '#b6f3f7', '#f0f6fc'],
  },
} as const;

export type PaletteKey = keyof typeof PALETTES;

/**
 * 增亮直接作用在颜色值上，不用 CSS filter：filter 会让合成器重新读取 WebGL 画布，
 * 而 xterm 的 WebGL 渲染器默认不保留绘制缓冲区，读回来就是脏帧（整屏花屏）。
 * 用伽马提升而不是乘系数：乘系数会把已经很亮的颜色顶到 255、色相跟着歪（浅蓝变青、前景洗白），
 * 伽马是暗色抬得多、亮色几乎不动。
 */
const GAMMA = [1, 1.15, 1.3, 1.5];

function boost(css: string, g: number) {
  if (g === 1) return css;
  const n = parseInt(css.slice(1), 16);
  return '#' + [16, 8, 0]
    .map((s) => Math.round(255 * Math.pow(((n >> s) & 0xff) / 255, 1 / g)).toString(16).padStart(2, '0'))
    .join('');
}

export function termTheme(key: string, bright = 0) {
  const p = PALETTES[key as PaletteKey] ?? PALETTES.tango;
  const g = GAMMA[bright] ?? 1;
  const theme: Record<string, string> = {
    background: '#101010',
    foreground: boost(p.foreground, g),
    cursor: '#7fd1b9',
    selectionBackground: '#2d4f60',
  };
  p.ansi.forEach((c, i) => { theme[ANSI_KEYS[i]] = boost(c, g); });
  return theme;
}
