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

export function termTheme(key: string) {
  const p = PALETTES[key as PaletteKey] ?? PALETTES.tango;
  const theme: Record<string, string> = {
    background: '#101010',
    foreground: p.foreground,
    cursor: '#7fd1b9',
    selectionBackground: '#2d4f60',
  };
  p.ansi.forEach((c, i) => { theme[ANSI_KEYS[i]] = c; });
  return theme;
}
