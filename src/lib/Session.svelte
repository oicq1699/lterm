<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { SearchAddon } from '@xterm/addon-search';
  import { Unicode11Addon } from '@xterm/addon-unicode11';
  import { WebglAddon } from '@xterm/addon-webgl';
  import { readText } from '@tauri-apps/plugin-clipboard-manager';
  import '@xterm/xterm/css/xterm.css';
  import { termTheme } from './termPalette';
  import SftpBrowser from './SftpBrowser.svelte';

  // Ctrl+Enter 的改写目标：ESC+CR 等同 Alt+Enter（绝大多数 TUI 认这个），
  // CSI 13;5u 是 Kitty 键盘协议的编码，支持它的应用能真正区分出 Ctrl+Enter
  const CTRL_ENTER_BYTES: Record<string, string> = {
    newline: '\x1b\r',
    csiu: '\x1b[13;5u',
  };

  type Props = {
    sessionId: string;
    profileId: string;
    sessionPassword: string | null;
    fontFamily: string;
    active: boolean;
    fontSize: number;
    /** ANSI 调色板：tango | xterm | high */
    prefPalette: string;
    /** 最小对比度（WCAG），1=关闭 */
    prefMinContrast: number;
    prefCopyOnSelect: boolean;
    prefConfirmMultiLine: boolean;
    prefContextMenu: boolean;
    /** Ctrl+Enter 发送的字节：'newline'（等价 Alt+Enter）| 'csiu'（Kitty 编码）| 'same'（不拦截） */
    prefCtrlEnter: string;
    onClosed: () => void;
    onClone?: () => void;
    /** 面板收起/关闭时请求把焦点交还终端（xterm 输入框只能有一个持有者） */
    onFocusBack?: () => void;
    onResize: (cols: number, rows: number) => void;
    registerFit: (fn: () => void) => void;
    registerApi: (sid: string, api: { writeLine: (t: string) => void }) => void;
  };
  let {
    sessionId, profileId, sessionPassword, fontFamily, active, fontSize, prefPalette, prefMinContrast,
    prefCopyOnSelect, prefConfirmMultiLine,
    prefContextMenu, prefCtrlEnter, onClosed, onClone, onFocusBack, onResize, registerFit, registerApi,
  }: Props = $props();

  let term: Terminal | undefined;
  let termReady = $state(false);
  let webgl: WebglAddon | undefined;
  let fit: FitAddon | undefined;
  let searchAddon: SearchAddon | undefined;
  let unlistenAll: UnlistenFn[] = [];
  let termHost: HTMLDivElement | undefined = $state();
  let showSearch = $state(false);
  let showSftp = $state(false);
  let sftpW = $state(580);
  // 详情列（权限/所有者）随面板实际宽度出现，拖拽和「详情」按钮不会互相矛盾
  const sftpWide = $derived(sftpW >= 900);
  const sftpMinW = 420;
  const sftpMaxW = () => Math.max(sftpMinW, window.innerWidth - 80);
  const clampSftpW = (w: number) => Math.min(sftpMaxW(), Math.max(sftpMinW, w));

  function toggleSftpCols() {
    sftpW = clampSftpW(sftpWide ? 580 : 980);
  }

  function startSftpResize(e: PointerEvent) {
    e.preventDefault();
    const startX = e.clientX;
    const startW = sftpW;
    const move = (ev: PointerEvent) => { sftpW = clampSftpW(startW + (startX - ev.clientX)); };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }
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
    if (!sel) return;
    // 走 Rust 侧通道：webview 的 navigator.clipboard 在失焦/无权限时会静默失败
    try { await invoke('clipboard_write', { text: sel }); } catch { /* 忽略 */ }
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

  // ---- 右键菜单 ----
  const CTX_W = 152, CTX_H = 106;
  let ctx = $state<{ x: number; y: number } | null>(null);
  let ctxEl: HTMLDivElement | undefined = $state();
  let hasSel = $state(false);
  function openCtx(e: MouseEvent) {
    ctx = {
      x: Math.max(4, Math.min(e.clientX, window.innerWidth - CTX_W)),
      y: Math.max(4, Math.min(e.clientY, window.innerHeight - CTX_H)),
    };
  }
  function closeCtx() { ctx = null; }
  function onWinPointerDown(e: PointerEvent) {
    if (ctx && ctxEl && !ctxEl.contains(e.target as Node)) closeCtx();
  }
  function onWinKeydown(e: KeyboardEvent) { if (e.key === 'Escape') closeCtx(); }
  function doFit() {    fit?.fit();
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

  // 配色/对比度改动即时作用到已开着的终端（WebGL 的图集要清掉才会用新色重绘）
  $effect(() => {
    if (!termReady) return;
    term!.options.theme = termTheme(prefPalette);
    term!.options.minimumContrastRatio = prefMinContrast;
    try { webgl?.clearTextureAtlas(); } catch { /* 渲染器已降级 */ }
  });

  onMount(async () => {
    term = new Terminal({
      fontFamily: fontStack(),
      fontSize,
      scrollback: 10000,
      cursorBlink: true,
      allowProposedApi: true,
      theme: termTheme(prefPalette),
      minimumContrastRatio: prefMinContrast,
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

    // IME 接管（仅 Linux/WebKitGTK+fcitx）：fcitx 在后续候选提交时不发 compositionstart，
    // 且 xterm 与目标元素上的监听先于同元素后注册者执行——所以拦截必须挂在父节点(termHost)
    // 捕获阶段才能先于 xterm。compositionend.data 权威上屏，恰好发一次。
    //
    // Windows/WebView2 不启用接管：实测接管逻辑会让中文输入法卡死输入（切回英文也打不进），
    // 而 xterm 自带的 CompositionHelper 在 WebView2 下工作正常。
    const helperTa = term.element?.querySelector<HTMLTextAreaElement>('.xterm-helper-textarea');
    const imeTakeover = !/Windows/i.test(navigator.userAgent);
    let imeDbg = false;
    try { imeDbg = localStorage.getItem('lterm-ime-debug') === '1'; } catch { /* ignore */ }
    let compActive = false;
    let compBuffer = '';
    let lastCommitAt = 0;
    let last229At = 0;
    // 229 只作**有时效**的证据（每次 229 刷新），绝不永久置位：
    // 否则一次无配对的 229（如切输入法时的 Shift）就会把后续所有按键吞掉。
    const inCompWindow = () =>
      compActive || (Date.now() - last229At < 250) || (Date.now() - lastCommitAt < 150);
    let sendQ: Promise<void> = Promise.resolve();
    function sendText(s: string) {
      if (!term || dead) { if (imeDbg) console.warn('[ime] sendText skipped: term?', !!term, 'dead=', dead); return; }
      // 大内容分块串行写入：避免超大单条 IPC 被丢，也适配远端 pty 的反压
      const bytes = new TextEncoder().encode(s);
      const CHUNK = 24 * 1024;
      sendQ = sendQ.then(async () => {
        for (let i = 0; i < bytes.length; i += CHUNK) {
          const b64 = bytesToB64(bytes.subarray(i, Math.min(i + CHUNK, bytes.length)));
          await invoke('write_input', { id: sessionId, dataBase64: b64 });
        }
        if (imeDbg) console.log('[ime] write ok', bytes.length, 'bytes in', Math.max(1, Math.ceil(bytes.length / CHUNK)), 'chunks');
      }).catch((e) => { if (imeDbg) console.warn('[ime] write FAIL', String(e)); });
    }
    if (helperTa && imeTakeover) {
      const dbgEv = (name: string, e: Event) => {
        if (!imeDbg) return;
        const ev = e as InputEvent & KeyboardEvent & CompositionEvent;
        console.log('[ime]', name, JSON.stringify({
          key: ev.key, keyCode: ev.keyCode, inputType: ev.inputType,
          data: typeof ev.data === 'string' ? ev.data : undefined,
          compActive, ta: helperTa.value.slice(-10), ms: new Date().getMilliseconds(),
        }));
      };
      const isCompInput = (ev: InputEvent) =>
        ev.inputType === 'insertCompositionText' || ev.inputType === 'insertFromComposition';

      termHost!.addEventListener('compositionstart', (e) => {
        dbgEv('compositionstart', e);
        compActive = true; compBuffer = '';
        e.stopPropagation(); e.preventDefault();
      }, true);
      termHost!.addEventListener('compositionupdate', (e) => {
        dbgEv('compositionupdate', e);
        const d = (e as CompositionEvent).data;
        if (typeof d === 'string') compBuffer = d;
        e.stopPropagation(); e.preventDefault();
      }, true);
      termHost!.addEventListener('compositionend', (e) => {
        dbgEv('compositionend', e);
        e.stopPropagation();
        compActive = false;
        lastCommitAt = Date.now();
        const final = (e as CompositionEvent).data || compBuffer;
        compBuffer = '';
        helperTa.value = '';
        if (final) {
          if (imeDbg) console.log('[ime] COMMIT', JSON.stringify(final));
          sendText(final);
        }
      }, true);
      termHost!.addEventListener('keydown', (e) => {
        const ev = e as KeyboardEvent;
        if (ev.keyCode === 229) last229At = Date.now(); // 无 compositionstart 时的周期证据，仅 250ms 内有效
        if (inCompWindow()) { dbgEv('keydown(blocked)', ev); ev.stopPropagation(); ev.preventDefault(); }
      }, true);
      termHost!.addEventListener('keypress', (e) => {
        if (inCompWindow()) { dbgEv('keypress(blocked)', e); e.stopPropagation(); e.preventDefault(); }
      }, true);
      termHost!.addEventListener('beforeinput', (e) => {
        const ev = e as InputEvent;
        if (compActive || isCompInput(ev)) { dbgEv('beforeinput(blocked)', ev); ev.stopPropagation(); }
      }, true);
      termHost!.addEventListener('input', (e) => {
        const ev = e as InputEvent;
        if (compActive || isCompInput(ev)) {
          dbgEv('input(blocked)', ev);
          if (isCompInput(ev)) helperTa.value = '';
          ev.stopPropagation();
        }
      }, true);
      // 失焦即视为组合周期结束，避免 compositionend 缺失时状态卡死
      helperTa.addEventListener('blur', () => { compActive = false; compBuffer = ''; }, true);
    }
    // WebGL 渲染器：修复 DOM 渲染器下 TUI 边框竖线断续问题（不可用时自动回退）
    try {
      const g = new WebglAddon();
      g.onContextLoss(() => { try { g.dispose(); } catch { /* 已释放 */ } });
      term.loadAddon(g);
      webgl = g;
    } catch { webgl = undefined; /* 无 WebGL 环境，保持默认渲染器 */ }
    termReady = true;
    requestAnimationFrame(() => {
      doFit();
      term?.focus();
    });

    registerFit(() => { doFit(); term?.focus(); });
    registerApi(sessionId, { writeLine: (t: string) => term?.writeln(t) });

    term.onData((d) => {
      if (dead) return;
      if (imeDbg) console.log('[ime] onData', JSON.stringify(d), 'compActive=', compActive);
      sendText(d);
    });
    term.onSelectionChange(() => {
      hasSel = !!term?.hasSelection();
      if (prefCopyOnSelect) copySelection();
    });

    // 捕获阶段：xterm 处理 Escape 时会 stopPropagation，冒泡阶段的 window 监听收不到
    window.addEventListener('keydown', onWinKeydown, true);
    termHost!.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      const ev = e as MouseEvent;
      if (prefContextMenu) { openCtx(ev); return; }
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
      // Ctrl+Enter 在终端协议里和 Enter 是同一字节（\r），TUI 分不出来，
      // 所以按偏好改写成 Alt+Enter 的 ESC+CR，或 Kitty CSI-u 编码
      if (k === 'enter' && !e.shiftKey && !e.metaKey && prefCtrlEnter !== 'same') {
        e.preventDefault();
        sendText(CTRL_ENTER_BYTES[prefCtrlEnter] ?? '\x1b\r');
        return false;
      }
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

  // 图标按钮/面板关闭按钮：按下不抢焦点，收起面板后把焦点交还终端输入框
  const noFocus = (e: MouseEvent) => e.preventDefault();
  // 只在面板从“开”变“关”时回收焦点；active 等只懒读，避免切标签时误触发
  let prevSftp = false, prevFwd = false;
  $effect(() => {
    const s = showSftp, f = showFwd;
    const closed = (prevSftp && !s) || (prevFwd && !f);
    prevSftp = s; prevFwd = f;
    if (closed && !s && !f) {
      queueMicrotask(() => {
        if (onFocusBack) onFocusBack();
        else if (active) term?.focus();
      });
    }
  });

  onDestroy(() => {
    window.removeEventListener('keydown', onWinKeydown, true);
    for (const u of unlistenAll) u();
    term?.dispose();
  });
</script>

<svelte:window onpointerdown={onWinPointerDown} />
<div class="term-wrap" class:hidden={!active}>
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
    <div class="sftp-side" style:width={`${sftpW}px`}>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
      <div
        class="sftp-grip"
        role="separator"
        aria-orientation="vertical"
        aria-label="拖动调整面板宽度"
        tabindex="0"
        title="拖动调整宽度"
        onpointerdown={startSftpResize}
        onkeydown={(e) => {
          if (e.key === 'ArrowLeft') { sftpW = clampSftpW(sftpW + 40); e.preventDefault(); }
          else if (e.key === 'ArrowRight') { sftpW = clampSftpW(sftpW - 40); e.preventDefault(); }
        }}
      ></div>
      <div class="sftp-head">
        <span>SFTP 文件</span>
        <button onmousedown={noFocus} onclick={toggleSftpCols} title="显示/隐藏权限与所有者两列">{sftpWide ? '精简' : '详情'}</button>
        <button onmousedown={noFocus} onclick={() => showSftp = false} aria-label="关闭文件面板">✕</button>
      </div>
      <SftpBrowser sid={sessionId} {profileId} password={sessionPassword} wide={sftpWide} />
    </div>
  {/if}
  {#if showFwd}
    <div class="fwd-side" style:right={showSftp ? `${34 + sftpW}px` : '34px'}>
      <div class="sftp-head">
        <span>端口转发（本会话）</span>
        <button onmousedown={noFocus} onclick={() => showFwd = false} aria-label="关闭转发面板">✕</button>
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
  <div class="tools-rail">
    <button class="ptool" class:open={showSftp} onmousedown={noFocus} onclick={() => showSftp = !showSftp} title="文件传输面板">📁</button>
    <button class="ptool" class:open={showFwd} onmousedown={noFocus} onclick={() => showFwd = !showFwd} title="端口转发面板">⇄</button>
  </div>
  {#if ctx}
    <div class="ctxmenu" bind:this={ctxEl} role="menu" style:left={`${ctx.x}px`} style:top={`${ctx.y}px`}>
      <button role="menuitem" class:off={!hasSel} onmousedown={noFocus} onclick={() => { if (hasSel) void copySelection(); closeCtx(); }}>复制</button>
      <button role="menuitem" onmousedown={noFocus} onclick={() => { void pasteClipboard(); closeCtx(); }}>粘贴</button>
      {#if onClone}
        <button role="menuitem" onmousedown={noFocus} onclick={() => { onClone(); closeCtx(); }}>克隆此连接</button>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* 隐藏用 class 而不是 style:display：内联 display 会盖掉这里的 flex，竖排图标条会掉到终端下面 */
  .term-wrap { height: 100%; flex: 1; min-width: 0; position: relative; display: flex; }
  .term-wrap.hidden { display: none !important; }
  .ctxmenu { position: fixed; z-index: 60; min-width: 136px; background: #262626; border: 1px solid #454545; border-radius: 6px; padding: 4px; display: flex; flex-direction: column; gap: 1px; box-shadow: 0 6px 18px rgba(0, 0, 0, 0.55); }
  .ctxmenu button { display: block; width: 100%; text-align: left; background: none; border: none; color: #ddd; padding: 5px 10px; border-radius: 4px; cursor: pointer; font-size: 13px; }
  .ctxmenu button:hover { background: #363636; }
  .ctxmenu button.off { color: #666; cursor: default; }
  .ctxmenu button.off:hover { background: none; }
  .term-col { flex: 1; display: flex; flex-direction: column; min-width: 0; height: 100%; }
  .term { flex: 1; padding: 6px; min-height: 0; }
  .findbar { display: flex; gap: 4px; padding: 4px 8px; background: #222; border-bottom: 1px solid #333; }
  .findbar input { flex: 1; padding: 4px 8px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .findbar button { padding: 2px 10px; }
  /* 面板让开图标列（30px）+ 4px 缝，否则盖住图标右半边 */
  .sftp-side { position: absolute; right: 34px; top: 0; bottom: 0; width: 580px; min-width: 420px; border-left: 1px solid #333; display: flex; flex-direction: column; background: #161616; z-index: 20; box-shadow: -8px 0 20px rgba(0, 0, 0, 0.4); }
  .sftp-grip { position: absolute; left: -4px; top: 0; bottom: 0; width: 8px; cursor: ew-resize; z-index: 1; }
  .sftp-grip:hover, .sftp-grip:focus-visible { background: #8ab4f855; outline: none; }
  .sftp-head { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 4px 10px; border-bottom: 1px solid #2a2a2a; color: #999; font-size: 12px; }
  .sftp-head span { flex: 1; }
  .sftp-head button { background: none; border: none; color: #999; cursor: pointer; }
  .ptool { width: 100%; box-sizing: border-box; text-align: center; background: #222c; border: 1px solid #3a3a3a; color: #8ab4f8; cursor: pointer; border-radius: 6px; padding: 3px 0; font-size: 13px; }
  .ptool.open { color: #101010; background: #8ab4f8; }
  .ptool:hover { border-color: #8ab4f8; }
  /* 图标改竖排、与终端并排占独立一列（定宽 30px），不再悬在文字上方 */
  .tools-rail { flex-shrink: 0; width: 30px; display: flex; flex-direction: column; gap: 4px; padding: 6px 0 0; }
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
