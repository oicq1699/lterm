<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { SearchAddon } from '@xterm/addon-search';
  import { Unicode11Addon } from '@xterm/addon-unicode11';
  import { WebglAddon } from '@xterm/addon-webgl';
  import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager';
  import '@xterm/xterm/css/xterm.css';
  import SftpBrowser from './SftpBrowser.svelte';

  type Props = {
    sessionId: string;
    profileId: string;
    sessionPassword: string | null;
    fontFamily: string;
    active: boolean;
    fontSize: number;
    prefCopyOnSelect: boolean;
    prefConfirmMultiLine: boolean;
    onClosed: () => void;
    onResize: (cols: number, rows: number) => void;
    registerFit: (fn: () => void) => void;
    registerApi: (sid: string, api: { writeLine: (t: string) => void }) => void;
  };
  let {
    sessionId, profileId, sessionPassword, fontFamily, active, fontSize, prefCopyOnSelect, prefConfirmMultiLine,
    onClosed, onResize, registerFit, registerApi,
  }: Props = $props();

  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let searchAddon: SearchAddon | undefined;
  let unlistenAll: UnlistenFn[] = [];
  let termHost: HTMLDivElement | undefined = $state();
  let showSearch = $state(false);
  let showSftp = $state(false);
  let searchTerm = $state('');
  let dead = $state(false);

  function fontStack() {
    return fontFamily
      ? `"${fontFamily}", JetBrains Mono, Sarasa Mono SC, Microsoft YaHei Mono, monospace`
      : 'JetBrains Mono, Sarasa Mono SC, Microsoft YaHei Mono, monospace';
  }

  function b64ToBytes(s: string): Uint8Array {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  function bytesToB64(b: Uint8Array): string {
    let s = '';
    for (const x of b) s += String.fromCharCode(x);
    return btoa(s);
  }

  async function copySelection() {
    const sel = term?.getSelection();
    if (sel) await writeText(sel);
  }

  async function pasteClipboard() {
    if (!term || dead) return;
    let text: string;
    try { text = await readText(); } catch { return; }
    if (!text) return;
    if (prefConfirmMultiLine && /[\r\n]/.test(text)) {
      const lines = text.split(/\r?\n/).filter((l, i, a) => l !== '' || i < a.length - 1).length;
      if (!confirm(`剪贴板包含 ${lines} 行文本，确认粘贴到终端？`)) return;
    }
    term.paste(text);
  }

  function doFit() {
    fit?.fit();
    if (term && !dead) onResize(term.cols, term.rows);
  }

  onMount(async () => {
    term = new Terminal({
      fontFamily: fontStack(),
      fontSize,
      scrollback: 10000,
      cursorBlink: true,
      allowProposedApi: true,
      theme: {
        background: '#101010', foreground: '#d4d4d4', cursor: '#7fd1b9',
        selectionBackground: '#2d4f60',
      },
    });
    fit = new FitAddon();
    searchAddon = new SearchAddon();
    const u11 = new Unicode11Addon();
    term.loadAddon(fit);
    term.loadAddon(searchAddon);
    term.loadAddon(u11);
    term.unicode.activeVersion = '11';
    // OSC 52：zellij 等 TUI 请求写系统剪贴板（webview 的 navigator.clipboard 会被权限拦截）
    term.parser.registerOscHandler(52, (data) => {
      const semi = data.indexOf(';');
      const payload = semi >= 0 ? data.slice(semi + 1) : data;
      if (semi < 0 || payload === '' || payload === '?') return true; // 清空/查询请求：不支持，仅吞掉
      try {
        const text = new TextDecoder().decode(b64ToBytes(payload));
        invoke('clipboard_write', { text }).catch(() => {});
      } catch { /* base64 非法，忽略 */ }
      return true;
    });

    term.open(termHost!);
    // WebGL 渲染器：修复 DOM 渲染器下 TUI 边框竖线断续问题（不可用时自动回退）
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => { try { webgl.dispose(); } catch { /* 已释放 */ } });
      term.loadAddon(webgl);
    } catch { /* 无 WebGL 环境，保持默认渲染器 */ }
    requestAnimationFrame(() => {
      doFit();
      term?.focus();
    });

    registerFit(() => { doFit(); term?.focus(); });
    registerApi(sessionId, { writeLine: (t: string) => term?.writeln(t) });

    term.onData((d) => {
      if (!dead) invoke('write_input', { id: sessionId, dataBase64: bytesToB64(new TextEncoder().encode(d)) });
    });
    term.onSelectionChange(() => { if (prefCopyOnSelect) copySelection(); });

    termHost!.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      if (term?.hasSelection()) copySelection();
      else pasteClipboard();
    });
    termHost!.addEventListener('wheel', (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      // 交给父级统一调整字号（共享偏好）
      window.dispatchEvent(new CustomEvent('lterm-zoom', { detail: e.deltaY < 0 ? 1 : -1 }));
    }, { passive: false });

    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown' || !e.ctrlKey) return true;
      const k = e.key.toLowerCase();
      // preventDefault：屏蔽 WebView2 原生 Ctrl+Shift+C/V 快捷键，避免与手动 paste 叠加成双份
      if (e.shiftKey && k === 'c') { e.preventDefault(); copySelection(); return false; }
      if (e.shiftKey && k === 'v') { e.preventDefault(); pasteClipboard(); return false; }
      if (!e.shiftKey && k === 'f') { e.preventDefault(); showSearch = true; return false; }
      if (k === '=' || k === '+') { e.preventDefault(); window.dispatchEvent(new CustomEvent('lterm-zoom', { detail: 1 })); return false; }
      if (k === '-') { e.preventDefault(); window.dispatchEvent(new CustomEvent('lterm-zoom', { detail: -1 })); return false; }
      if (k === '0') { e.preventDefault(); window.dispatchEvent(new CustomEvent('lterm-zoom', { detail: 999 })); return false; }
      return true;
    });

    // 捕获阶段拦截原生 paste：xterm 隐藏输入框收到 paste 会再插一遍（与 term.paste 重复）
    termHost!.addEventListener('paste', (e) => { e.preventDefault(); e.stopPropagation(); }, true);

    unlistenAll.push(await listen<{ id: string; data: string }>('pty-output', (ev) => {
      if (ev.payload.id === sessionId) term?.write(b64ToBytes(ev.payload.data));
    }));
    unlistenAll.push(await listen<{ id: string; reason: string }>('pty-closed', (ev) => {
      if (ev.payload.id !== sessionId) return;
      dead = true;
      term?.writeln('\r\n\x1b[33m' + ev.payload.reason + '\x1b[0m');
      onClosed();
    }));
  });

  // active 切换回本会话时重新 fit
  $effect(() => { if (active) requestAnimationFrame(doFit); });

  // 字号/字体变化同步
  $effect(() => {
    if (term) {
      term.options.fontSize = fontSize;
      term.options.fontFamily = fontStack();
      if (active) doFit();
    }
  });

  function closeSearch() {
    showSearch = false;
    searchTerm = '';
    searchAddon?.clearDecorations();
    term?.focus();
  }

  onDestroy(() => {
    for (const u of unlistenAll) u();
    term?.dispose();
  });
</script>

<div class="term-wrap" style:display={active ? 'flex' : 'none'}>
  <div class="term-col">
    {#if showSearch}
      <div class="findbar">
        <input placeholder="搜索输出内容…" bind:value={searchTerm}
               aria-label="搜索输出内容"
               onkeydown={(e) => {
                 if (e.key === 'Enter') e.shiftKey ? searchAddon?.findNext(searchTerm) : searchAddon?.findPrevious(searchTerm);
                 if (e.key === 'Escape') closeSearch();
               }} />
        <button onclick={() => searchAddon?.findPrevious(searchTerm)}>↑</button>
        <button onclick={() => searchAddon?.findNext(searchTerm)}>↓</button>
        <button onclick={closeSearch}>✕</button>
      </div>
    {/if}
    <div class="term" bind:this={termHost}></div>
  </div>
  {#if showSftp}
    <div class="sftp-side">
      <div class="sftp-head">
        <span>SFTP 文件</span>
        <button onclick={() => showSftp = false} aria-label="关闭文件面板">✕</button>
      </div>
      <SftpBrowser sid={sessionId} {profileId} password={sessionPassword} />
    </div>
  {/if}
  <button class="sftp-toggle" class:open={showSftp} onclick={() => showSftp = !showSftp} title="文件传输面板">📁</button>
</div>

<style>
  .term-wrap { height: 100%; flex: 1; min-width: 0; position: relative; }
  .term-col { flex: 1; display: flex; flex-direction: column; min-width: 0; height: 100%; }
  .term { flex: 1; padding: 6px; min-height: 0; }
  .findbar { display: flex; gap: 4px; padding: 4px 8px; background: #222; border-bottom: 1px solid #333; }
  .findbar input { flex: 1; padding: 4px 8px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .findbar button { padding: 2px 10px; }
  .sftp-side { position: absolute; right: 0; top: 0; bottom: 0; width: 580px; min-width: 420px; border-left: 1px solid #333; display: flex; flex-direction: column; background: #161616; z-index: 20; box-shadow: -8px 0 20px rgba(0, 0, 0, 0.4); }
  .sftp-head { display: flex; justify-content: space-between; align-items: center; padding: 4px 10px; border-bottom: 1px solid #2a2a2a; color: #999; font-size: 12px; }
  .sftp-head button { background: none; border: none; color: #999; cursor: pointer; }
  .sftp-toggle { position: absolute; top: 6px; right: 8px; z-index: 21; background: #222c; border: 1px solid #333; color: #8ab4f8; cursor: pointer; border-radius: 6px; padding: 2px 8px; }
  .sftp-toggle.open { color: #101010; background: #8ab4f8; }
</style>
