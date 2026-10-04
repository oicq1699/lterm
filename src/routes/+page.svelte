<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { SearchAddon } from '@xterm/addon-search';
  import { Unicode11Addon } from '@xterm/addon-unicode11';
  import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager';
  import '@xterm/xterm/css/xterm.css';

  type Profile = {
    id: string; name: string; host: string; port: number; username: string;
    auth_method: 'password' | 'key' | 'agent'; key_path: string | null;
    remark: string; group_tag: string | null; color: string | null;
    created_at: number; updated_at: number;
  };
  type Hit = { server: Profile; score: number };

  let servers: Profile[] = $state([]);
  let hits: Hit[] = $state([]);
  let query = $state('');
  let showForm = $state(false);
  let draft: Profile = $state(emptyDraft());
  let error = $state('');
  let connectedId = $state<string | null>(null);
  let closedReason = $state('');
  let lastProfile = $state<Profile | null>(null);

  // ---- 用户偏好（持久化到 localStorage）----
  let prefCopyOnSelect = $state((localStorage.getItem('prefCopyOnSelect') ?? '1') === '1');
  let prefConfirmMultiLine = $state((localStorage.getItem('prefConfirmMultiLine') ?? '1') === '1');
  let fontSize = $state(Number(localStorage.getItem('fontSize') ?? 14));
  let showSettings = $state(false);
  let showSearch = $state(false);
  let searchTerm = $state('');

  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let searchAddon: SearchAddon | undefined;
  let unlistenAll: UnlistenFn[] = [];
  let termHost: HTMLDivElement | undefined = $state();

  function emptyDraft(): Profile {
    return { id: '', name: '', host: '', port: 22, username: 'root',
      auth_method: 'agent', key_path: null, remark: '', group_tag: null, color: null,
      created_at: 0, updated_at: 0 };
  }

  async function refresh() {
    servers = await invoke<Profile[]>('list_servers');
    search();
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function search() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      hits = query.trim()
        ? await invoke<Hit[]>('search_servers', { query })
        : servers.map((s) => ({ server: s, score: 0 }));
    }, 80);
  }

  async function save() {
    try {
      await invoke('upsert_server', { profile: draft });
      showForm = false;
      draft = emptyDraft();
      await refresh();
    } catch (e) { error = String(e); }
  }

  function edit(p: Profile) {
    draft = { ...p };
    showForm = true;
  }

  async function remove(id: string) {
    if (!confirm('删除该服务器？')) return;
    try {
      await invoke('delete_server', { id });
      await refresh();
    } catch (e) { error = String(e); }
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

  // ---- 剪贴板 ----
  async function copySelection() {
    const sel = term?.getSelection();
    if (sel) await writeText(sel);
  }

  async function pasteClipboard() {
    if (!term || !connectedId) return;
    let text: string;
    try { text = await readText(); } catch { return; }
    if (!text) return;
    if (prefConfirmMultiLine && /[\r\n]/.test(text)) {
      const lines = text.split(/\r?\n/).filter((l, i, a) => l !== '' || i < a.length - 1).length;
      if (!confirm(`剪贴板包含 ${lines} 行文本，确认粘贴到终端？`)) return;
    }
    term.paste(text);
  }

  function doReconnect() {
    if (lastProfile) connect(lastProfile);
  }

  async function connect(p: Profile) {
    error = '';
    if (connectedId) { error = '已有活动会话，请先断开'; return; }
    const cols = term?.cols ?? 80;
    const rows = term?.rows ?? 24;
    const password = p.auth_method === 'password' ? prompt('输入密码（不落盘）') : null;
    try {
      await invoke('connect', { profileId: p.id, password, cols, rows });
      connectedId = p.id;
      lastProfile = p;
      closedReason = '';
      term?.focus();
    } catch (e) { error = String(e); }
  }

  async function disconnect() {
    if (!connectedId) return;
    await invoke('disconnect', { id: connectedId });
    connectedId = null;
  }

  function setFont(size: number) {
    fontSize = Math.min(28, Math.max(10, size));
    if (term) {
      term.options.fontSize = fontSize;
      fit?.fit();
      if (connectedId && term) invoke('resize', { id: connectedId, cols: term.cols, rows: term.rows });
    }
    localStorage.setItem('fontSize', String(fontSize));
  }

  onMount(async () => {
    term = new Terminal({
      fontFamily: 'JetBrains Mono, Sarasa Mono SC, Microsoft YaHei Mono, monospace',
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
    term.open(termHost!);
    fit.fit();

    term.onData((d) => {
      if (connectedId) invoke('write_input', { id: connectedId, dataBase64: bytesToB64(new TextEncoder().encode(d)) });
    });

    // 选中即复制（PuTTY 风格）
    term.onSelectionChange(() => {
      if (prefCopyOnSelect) copySelection();
    });

    // 右键：有选区→复制，无选区→粘贴
    termHost!.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      if (term?.hasSelection()) copySelection();
      else pasteClipboard();
    });
    // Ctrl+滚轮缩放
    termHost!.addEventListener('wheel', (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      setFont(fontSize + (e.deltaY < 0 ? 1 : -1));
    }, { passive: false });

    // 快捷键：Ctrl+Shift+C/V、Ctrl+F、Ctrl+滚轮外的加减号
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown' || !e.ctrlKey) return true;
      const k = e.key.toLowerCase();
      if (e.shiftKey && k === 'c') { copySelection(); return false; }
      if (e.shiftKey && k === 'v') { pasteClipboard(); return false; }
      if (!e.shiftKey && k === 'f') { showSearch = true; return false; }
      if (k === '=' || k === '+') { setFont(fontSize + 1); return false; }
      if (k === '-') { setFont(fontSize - 1); return false; }
      if (k === '0') { setFont(14); return false; }
      return true;
    });

    window.addEventListener('resize', onWinResize);

    unlistenAll.push(await listen<{ id: string; data: string }>('pty-output', (ev) => {
      if (ev.payload.id === connectedId) term?.write(b64ToBytes(ev.payload.data));
    }));
    unlistenAll.push(await listen<{ id: string; reason: string }>('pty-closed', (ev) => {
      if (ev.payload.id !== connectedId) return;
      connectedId = null;
      closedReason = ev.payload.reason;
      term?.writeln('\r\n\x1b[33m' + ev.payload.reason + '\x1b[0m');
    }));
    unlistenAll.push(await listen<{ host: string; port: number; fingerprint: string }>('hostkey-new', (ev) => {
      term?.writeln(`\r\n\x1b[33m[安全] 新主机 ${ev.payload.host}:${ev.payload.port}，指纹已记录: ${ev.payload.fingerprint}\x1b[0m`);
    }));
    unlistenAll.push(await listen<{ host: string; port: number; fingerprint: string }>('hostkey-mismatch', (ev) => {
      const msg = `主机密钥与记录不一致 ${ev.payload.host}:${ev.payload.port} (${ev.payload.fingerprint})，连接已拒绝`;
      error = msg;
      term?.writeln(`\r\n\x1b[31m[安全警告] ${msg}\x1b[0m`);
    }));

    await refresh();
  });

  function onWinResize() {
    fit?.fit();
    if (connectedId && term) invoke('resize', { id: connectedId, cols: term.cols, rows: term.rows });
  }

  function closeSearch() {
    showSearch = false;
    searchTerm = '';
    searchAddon?.clearDecorations();
    term?.focus();
  }

  $effect(() => {
    localStorage.setItem('prefCopyOnSelect', prefCopyOnSelect ? '1' : '0');
    localStorage.setItem('prefConfirmMultiLine', prefConfirmMultiLine ? '1' : '0');
  });

  onDestroy(() => {
    window.removeEventListener('resize', onWinResize);
    for (const u of unlistenAll) u();
    term?.dispose();
  });

  const authLabel = { password: '密码', key: '密钥', agent: 'agent' } as const;
</script>

<main>
  <aside>
    <div class="toolbar">
      <input class="search" placeholder="模糊搜索：名称 备注 用户 主机…" bind:value={query} oninput={search} />
      <button onclick={() => { showForm = !showForm; }} title="添加服务器">＋</button>
    </div>
    {#if showForm}
      <form class="server-form" onsubmit={(e) => { e.preventDefault(); save(); }}>
        <input placeholder="标题" bind:value={draft.name} required />
        <div class="row">
          <input placeholder="主机" bind:value={draft.host} required />
          <input class="port" type="number" placeholder="22" bind:value={draft.port} />
        </div>
        <input placeholder="用户名" bind:value={draft.username} required />
        <div class="row">
          <select bind:value={draft.auth_method}>
            <option value="agent">ssh-agent</option>
            <option value="key">私钥</option>
            <option value="password">密码</option>
          </select>
          {#if draft.auth_method === 'key'}
            <input placeholder="私钥路径" bind:value={draft.key_path} />
          {/if}
        </div>
        <input placeholder="备注" bind:value={draft.remark} />
        <div class="row">
          <button type="submit">保存</button>
          <button type="button" onclick={() => { showForm = false; draft = emptyDraft(); }}>取消</button>
        </div>
      </form>
    {/if}
    <ul class="server-list">
      {#each hits as hit (hit.server.id)}
        <li>
          <button class="server" onclick={() => connect(hit.server)} ondblclick={() => edit(hit.server)}
                  title="单击连接 · 双击编辑">
            <span class="name">{hit.server.name}</span>
            <span class="meta">{hit.server.username}@{hit.server.host} · {authLabel[hit.server.auth_method]}</span>
            {#if hit.server.remark}<span class="remark">{hit.server.remark}</span>{/if}
          </button>
          <button class="del" onclick={() => remove(hit.server.id)} title="删除">×</button>
        </li>
      {:else}
        <li class="empty">暂无服务器，点 ＋ 添加</li>
      {/each}
    </ul>
  </aside>

  <section class="term-pane">
    {#if error}<div class="error">{error}</div>{/if}
    {#if showSearch}
      <div class="findbar">
        <input placeholder="搜索输出内容…" bind:value={searchTerm}
               autofocus
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
    <div class="status">
      {#if connectedId}
        <span class="dot connected"></span>已连接
        <button onclick={disconnect}>断开</button>
      {:else if lastProfile && closedReason}
        <span class="dot closed"></span>{closedReason}
        <button onclick={doReconnect}>重连 {lastProfile.name}</button>
      {:else}
        <span class="dot"></span>未连接
      {/if}
      <span class="spacer"></span>
      <button class="gear" onclick={() => showSettings = !showSettings} title="终端设置">⚙</button>
      {#if showSettings}
        <div class="settings">
          <label><input type="checkbox" bind:checked={prefCopyOnSelect} /> 选中即复制</label>
          <label><input type="checkbox" bind:checked={prefConfirmMultiLine} /> 多行粘贴确认</label>
          <div class="row">
            <span>字号 {fontSize}</span>
            <button onclick={() => setFont(fontSize - 1)}>−</button>
            <button onclick={() => setFont(fontSize + 1)}>＋</button>
          </div>
          <div class="hint">右键：复制选区/粘贴 · Ctrl+Shift+C/V · Ctrl+F 搜索 · Ctrl+滚轮 缩放</div>
        </div>
      {/if}
    </div>
  </section>
</main>

<style>
  main { display: flex; height: 100vh; font-family: system-ui, sans-serif; }
  aside { width: 300px; border-right: 1px solid #333; display: flex; flex-direction: column; background: #1e1e1e; color: #ddd; }
  .toolbar { display: flex; gap: 6px; padding: 8px; }
  .search { flex: 1; padding: 6px; border-radius: 6px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .toolbar button { padding: 0 10px; }
  .server-form { display: flex; flex-direction: column; gap: 6px; padding: 8px; border-bottom: 1px solid #333; }
  .server-form .row { display: flex; gap: 6px; }
  .server-form input, .server-form select, .server-form button { padding: 5px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .port { width: 70px; }
  .server-list { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; }
  .server-list li { display: flex; align-items: stretch; }
  .server { flex: 1; text-align: left; background: none; border: none; border-bottom: 1px solid #2a2a2a; color: #ddd; padding: 8px 10px; cursor: pointer; display: flex; flex-direction: column; gap: 2px; }
  .server:hover { background: #2d2d2d; }
  .name { font-weight: 600; }
  .meta { font-size: 12px; color: #888; }
  .remark { font-size: 12px; color: #6a9fb5; }
  .del { border: none; background: none; color: #666; cursor: pointer; }
  .del:hover { color: #e57373; }
  .empty { padding: 16px; color: #666; }
  .term-pane { flex: 1; display: flex; flex-direction: column; background: #101010; position: relative; }
  .term { flex: 1; padding: 6px; }
  .error { padding: 6px 10px; background: #4a1d1d; color: #f0a0a0; font-size: 13px; }
  .findbar { display: flex; gap: 4px; padding: 4px 8px; background: #222; border-bottom: 1px solid #333; }
  .findbar input { flex: 1; padding: 4px 8px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .findbar button { padding: 2px 10px; }
  .status { padding: 4px 10px; font-size: 12px; color: #999; border-top: 1px solid #333; display: flex; gap: 10px; align-items: center; position: relative; }
  .status button { padding: 2px 10px; }
  .spacer { flex: 1; }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #555; margin-right: 2px; }
  .dot.connected { background: #4caf50; }
  .dot.closed { background: #e6a23c; }
  .gear { background: none; border: none; color: #999; cursor: pointer; font-size: 14px; }
  .settings { position: absolute; bottom: 28px; right: 8px; background: #252525; border: 1px solid #444; border-radius: 8px; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; z-index: 10; min-width: 230px; color: #ccc; }
  .settings label { display: flex; gap: 6px; align-items: center; cursor: pointer; }
  .settings .row { display: flex; gap: 8px; align-items: center; }
  .settings .hint { font-size: 11px; color: #777; }
</style>
