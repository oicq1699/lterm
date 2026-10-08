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
    profileColor: string | null;
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
    sessionId, profileId, sessionPassword, fontFamily, profileColor, active, fontSize, prefCopyOnSelect, prefConfirmMultiLine,
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

  // ---- 端口转发 ----
  type Fwd = { local: number; host: string; port: number };
  let showFwd = $state(false);
  let forwards = $state<Fwd[]>([]);
  let fwdLocal = $state('');
  let fwdHost = $state('');
  let fwdRemote = $state('');
  let fwdError = $state('');

  async function refreshForwards() {
    try { forwards = await invoke<Fwd[]>('forward_list', { id: sessionId }); } catch { forwards = []; }
  }
  async function addForward() {
    fwdError = '';
    const local = Number(fwdLocal), port = Number(fwdRemote);
    if (!local || !port || !fwdHost.trim()) { fwdError = '请填写本地端口 / 目标主机 / 目标端口'; return; }
    try {
      await invoke('forward_add', { id: sessionId, local, host: fwdHost.trim(), port });
      fwdLocal = ''; fwdHost = ''; fwdRemote = '';
      await refreshForwards();
    } catch (e) { fwdError = String(e); }
  }
  async function delForward(local: number) {
    fwdError = '';
    try { await invoke('forward_del', { id: sessionId, local }); } catch (e) { fwdError = String(e); }
    await refreshForwards();
  }
  $effect(() => { if (showFwd) void refreshForwards(); });

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

    // WebView2+微软拼音：
    // 1) 连续上屏时辅助输入框累积旧提交 → 组合结束后清空，杜绝滑动窗口式重复。
    // 2) 空格/回车/数字选词时 xterm 会在 keydown 立即发送一次、compositionend 又延迟发送一次
    //    （textarea 残留）→ 我们在 compositionend 同步清空 textarea，让第二次发送取到空串被跳过。
    const helperTa = term.element?.querySelector<HTMLTextAreaElement>('.xterm-helper-textarea');
    if (helperTa) {
      let composing = false;
      let keySelectAt = 0;
      helperTa.addEventListener('compositionstart', () => { composing = true; });
      helperTa.addEventListener('keydown', (e) => {
        const c = e.keyCode;
        if (composing && (c === 32 || c === 13 || (c >= 48 && c <= 57) || (c >= 96 && c <= 105))) {
          keySelectAt = Date.now();
        }
      });
      helperTa.addEventListener('compositionupdate', () => { keySelectAt = 0; });
      helperTa.addEventListener('compositionend', () => {
        composing = false;
        if (keySelectAt && Date.now() - keySelectAt < 60) {
          helperTa.value = ''; // xterm 的重复发送在其 0ms 定时器里取到空串即被跳过
        }
        keySelectAt = 0;
        setTimeout(() => { if (!composing) helperTa.value = ''; }, 20);
      });
    }
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
      // F12 优先拦截：否则 xterm 把功能键当转义序列发送并吞掉事件，window 层监听收不到
      if (e.type === 'keydown' && e.key === 'F12') {
        e.preventDefault();
        invoke('toggle_devtools').catch(() => {});
        return false;
      }
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

    // 注意：不要拦截/停止传播 paste 事件——中文 IME 候选词上屏在 WebView2 下经由 paste 提交，
    // 拦截会导致 xterm 隐藏输入框不清空、后续输入整串重复。双份粘贴已由 keydown preventDefault 解决。

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
    {#if profileColor}<div class="accent" style:background={profileColor}></div>{/if}
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
  {#if showFwd}
    <div class="fwd-side" style:right={showSftp ? '580px' : '0'}>
      <div class="sftp-head">
        <span>端口转发（本会话）</span>
        <button onclick={() => showFwd = false} aria-label="关闭转发面板">✕</button>
      </div>
      <div class="fwd-form">
        <div class="frow"><label for="fl-{sessionId}">本地端口</label><input id="fl-{sessionId}" type="number" placeholder="如 8080" bind:value={fwdLocal} /></div>
        <div class="frow"><label for="fh-{sessionId}">目标主机</label><input id="fh-{sessionId}" placeholder="远端可达地址，如 127.0.0.1" bind:value={fwdHost} /></div>
        <div class="frow"><label for="fp-{sessionId}">目标端口</label><input id="fp-{sessionId}" type="number" placeholder="如 3306" bind:value={fwdRemote} /></div>
        <button class="fadd" onclick={addForward}>添加转发</button>
      </div>
      {#if fwdError}<div class="fwd-err">{fwdError} <button onclick={() => fwdError = ''} aria-label="关闭提示">✕</button></div>{/if}
      <ul class="fwd-list">
        {#each forwards as f (f.local)}
          <li>
            <div class="fwd-info">
              <div class="fwd-main">127.0.0.1:{f.local} → {f.host}:{f.port}</div>
              <div class="fwd-sub">经当前 SSH 会话转发</div>
            </div>
            <button onclick={() => delForward(f.local)}>停止</button>
          </li>
        {:else}
          <li class="fwd-empty">暂无转发。把本机端口映射到远端可达的地址，等效 ssh -L；关闭标签自动停止。</li>
        {/each}
      </ul>
    </div>
  {/if}
  <div class="tool-stack" style:right={`${8 + (showSftp ? 580 : 0) + (showFwd ? 360 : 0)}px`}>
    <button class="ptool" class:open={showSftp} onclick={() => showSftp = !showSftp} title="文件传输面板">📁</button>
    <button class="ptool" class:open={showFwd} onclick={() => showFwd = !showFwd} title="端口转发面板">⇄</button>
  </div>
</div>

<style>
  .term-wrap { height: 100%; flex: 1; min-width: 0; position: relative; }
  .term-col { flex: 1; display: flex; flex-direction: column; min-width: 0; height: 100%; }
  .accent { height: 3px; flex-shrink: 0; }
  .term { flex: 1; padding: 6px; min-height: 0; }
  .findbar { display: flex; gap: 4px; padding: 4px 8px; background: #222; border-bottom: 1px solid #333; }
  .findbar input { flex: 1; padding: 4px 8px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .findbar button { padding: 2px 10px; }
  .sftp-side { position: absolute; right: 0; top: 0; bottom: 0; width: 580px; min-width: 420px; border-left: 1px solid #333; display: flex; flex-direction: column; background: #161616; z-index: 20; box-shadow: -8px 0 20px rgba(0, 0, 0, 0.4); }
  .sftp-head { display: flex; justify-content: space-between; align-items: center; padding: 4px 10px; border-bottom: 1px solid #2a2a2a; color: #999; font-size: 12px; }
  .sftp-head button { background: none; border: none; color: #999; cursor: pointer; }
  .ptool { background: #222c; border: 1px solid #3a3a3a; color: #8ab4f8; cursor: pointer; border-radius: 6px; padding: 3px 9px; font-size: 13px; }
  .ptool.open { color: #101010; background: #8ab4f8; }
  .ptool:hover { border-color: #8ab4f8; }
  .tool-stack { position: absolute; top: 6px; z-index: 23; display: flex; gap: 6px; }
  .fwd-side { position: absolute; top: 0; bottom: 0; width: 360px; border-left: 1px solid #333; display: flex; flex-direction: column; background: #161616; z-index: 22; box-shadow: -8px 0 20px rgba(0, 0, 0, 0.4); }
  .fwd-form { display: flex; flex-direction: column; gap: 6px; padding: 8px 10px; border-bottom: 1px solid #2a2a2a; }
  .frow { display: flex; align-items: center; gap: 8px; }
  .frow label { width: 56px; color: #aaa; font-size: 12px; flex-shrink: 0; }
  .frow input { flex: 1; min-width: 0; padding: 4px 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; font-size: 12px; }
  .fadd { padding: 5px 12px; border-radius: 5px; border: 1px solid #4c8bf5; background: #233a5c; color: #cfe1ff; cursor: pointer; font-size: 12px; }
  .fadd:hover { background: #2c4870; }
  .fwd-err { display: flex; justify-content: space-between; padding: 4px 8px; background: #4a1d1d; color: #f0a0a0; font-size: 12px; }
  .fwd-err button { background: none; border: none; color: #f0a0a0; cursor: pointer; }
  .fwd-list { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; }
  .fwd-list li { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 7px 10px; border-bottom: 1px solid #262626; }
  .fwd-list li:hover { background: #1d1d1d; }
  .fwd-info { min-width: 0; }
  .fwd-main { font-size: 12px; color: #ddd; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fwd-sub { font-size: 11px; color: #777; margin-top: 1px; }
  .fwd-list li button { padding: 2px 10px; font-size: 11px; border-radius: 4px; border: 1px solid #444; background: #2a2a2a; color: #ccc; cursor: pointer; }
  .fwd-list li button:hover { color: #e57373; border-color: #e57373; }
  .fwd-empty { color: #666; font-size: 12px; padding: 10px; display: block; line-height: 1.6; border-bottom: none !important; }
</style>
