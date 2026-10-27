/** 终端 16 色 ANSI 调色板；xterm.js 自带的是 Tango 系，暗背景上绿/黄/蓝偏暗。 */

const ANSI_KEYS = [
  'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite',
] as const;

type Scheme = { background: string; foreground: string; ansi: readonly string[] };

export const PALETTES: Record<string, Scheme> = {
  // xterm.js 默认（Tango 系）
  tango: {
    background: '#101010', foreground: '#d4d4d4',
    ansi: ['#2e3436', '#cc0000', '#4e9a06', '#c4a000', '#3465a4', '#75507b', '#06989a', '#d3d7cf',
      '#555753', '#ef2929', '#8ae234', '#fce94f', '#729fcf', '#ad7fa8', '#34e2e2', '#eeeeec'],
  },
  // 与真正的 xterm(1) 一致的 XT-16：常规色更饱和更亮，蓝/紫偏深
  xterm: {
    background: '#101010', foreground: '#e5e5e5',
    ansi: ['#000000', '#cd0000', '#00cd00', '#cdcd00', '#0000ee', '#cd00cd', '#00cdcd', '#e5e5e5',
      '#7f7f7f', '#ff0000', '#00ff00', '#ffff00', '#5c5cff', '#ff00ff', '#00ffff', '#ffffff'],
  },
  // 高对比：把暗背景上读不清的蓝/紫/黑整体提亮
  high: {
    background: '#101010', foreground: '#f0f6fc',
    ansi: ['#6e7681', '#ff7b72', '#7ee787', '#e3b341', '#79c0ff', '#d2a8ff', '#56d4dd', '#c9d1d9',
      '#8b949e', '#ffa198', '#aff5b4', '#f2cc60', '#a5d6ff', '#e2c5ff', '#b6f3f7', '#f0f6fc'],
  },
  // Windows Terminal / VS Code 默认
  campbell: {
    background: '#0c0c0c', foreground: '#cccccc',
    ansi: ['#0c0c0c', '#c50f1f', '#13a10e', '#c19c00', '#0037da', '#881798', '#3a96dd', '#cccccc',
      '#767676', '#e74856', '#16c60c', '#f9f1a5', '#3b78ff', '#b4009e', '#61d6d6', '#f2f2f2'],
  },
  gruvbox: {
    background: '#1d2021', foreground: '#ebdbb2',
    ansi: ['#282828', '#cc241d', '#98971a', '#d79921', '#458588', '#b16286', '#689d6a', '#a89984',
      '#928374', '#fb4934', '#b8bb26', '#fabd2f', '#83a598', '#d3869b', '#8ec07c', '#ebdbb2'],
  },
  dracula: {
    background: '#282a36', foreground: '#f8f8f2',
    ansi: ['#21222c', '#ff5555', '#50fa7b', '#f1fa8c', '#bd93f9', '#ff79c6', '#8be9fd', '#f8f8f2',
      '#44475a', '#ff6e6e', '#69ff94', '#ffffa5', '#d6acff', '#ff92df', '#a4ffff', '#ffffff'],
  },
  solarized: {
    background: '#002b36', foreground: '#93a1a1',
    ansi: ['#073642', '#dc322f', '#859900', '#b58900', '#268bd2', '#d33682', '#2aa198', '#eee8d5',
      '#002b36', '#cb4b16', '#586e75', '#657b83', '#839496', '#6c71c4', '#93a1a1', '#fdf6e3'],
  },
  catppuccin: {
    background: '#1e1e2e', foreground: '#cdd6f4',
    ansi: ['#45475a', '#f38ba8', '#a6e3a1', '#f9e2af', '#89b4fa', '#f5c2ef', '#94e2d5', '#bac2de',
      '#585b70', '#f38ba8', '#a6e3a1', '#f9e2af', '#89b4fa', '#f5c2ef', '#94e2d5', '#a6adc8'],
  },
};

export const PALETTE_LABELS: Record<string, string> = {
  tango: '默认（Tango）',
  xterm: 'xterm 经典',
  high: '高对比',
  campbell: 'Campbell（VS Code / Windows Terminal）',
  gruvbox: 'Gruvbox 暗',
  dracula: 'Dracula',
  solarized: 'Solarized 暗',
  catppuccin: 'Catppuccin Mocha',
};

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
  const p = PALETTES[key] ?? PALETTES.tango;
  const g = GAMMA[bright] ?? 1;
  const theme: Record<string, string> = {
    // 背景不参与增亮：抬背景会把文字对比度反噬回去，弱屏要的本来就是字够不够亮
    background: p.background,
    foreground: boost(p.foreground, g),
    cursor: '#7fd1b9',
    selectionBackground: '#2d4f60',
  };
  p.ansi.forEach((c, i) => { theme[ANSI_KEYS[i]] = boost(c, g); });
  return theme;
}
