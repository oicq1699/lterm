<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import Session from '$lib/Session.svelte';

  type Profile = {
    id: string; name: string; host: string; port: number; username: string;
    auth_method: 'password' | 'key' | 'agent'; key_path: string | null;
    remark: string; group_tag: string | null; color: string | null;
    created_at: number; updated_at: number;
  };
  type Hit = { server: Profile; score: number };
  type Tab = { sid: string; profile: Profile; alive: boolean; password: string | null };

  let servers: Profile[] = $state([]);
  let hits: Hit[] = $state([]);
  let query = $state('');
  let showForm = $state(false);
  let draft: Profile = $state(emptyDraft());
  let error = $state('');

  let tabs = $state<Tab[]>([]);
  let activeIdx = $state(-1);
  let paneEl: HTMLDivElement | undefined = $state();
  let fitFns = new Map<string, () => void>();
  let tabApis = new Map<string, { writeLine: (t: string) => void }>();
  let noticeQueue: string[] = [];

  // ---- 偏好 ----
  let prefCopyOnSelect = $state((localStorage.getItem('prefCopyOnSelect') ?? '1') === '1');
  let prefConfirmMultiLine = $state((localStorage.getItem('prefConfirmMultiLine') ?? '1') === '1');
  let fontSize = $state(Number(localStorage.getItem('fontSize') ?? 14));
  let fontFamily = $state(localStorage.getItem('fontFamily') ?? '');
  let fontList = $state<string[] | null>(null);
  let showSettings = $state(false);
  let showAside = $state(localStorage.getItem('asideHidden') !== '1');

  let unlistenAll: UnlistenFn[] = [];

  const activeTab = $derived(activeIdx >= 0 && activeIdx < tabs.length ? tabs[activeIdx] : null);

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

  function estimateSize() {
    const w = paneEl?.clientWidth ?? 800;
    const h = paneEl?.clientHeight ?? 600;
    return {
      cols: Math.max(20, Math.floor((w - 12) / (fontSize * 0.62))),
      rows: Math.max(6, Math.floor((h - 12) / (fontSize * 1.21))),
    };
  }

  async function connect(p: Profile) {
    error = '';
    const { cols, rows } = estimateSize();
    const password = p.auth_method === 'password' ? prompt('输入密码（不落盘）') : null;
    try {
      const sid = await invoke<string>('connect', { profileId: p.id, password, cols, rows });
      tabs = [...tabs, { sid, profile: p, alive: true, password }];
      activeIdx = tabs.length - 1;
      flushNotices();
    } catch (e) {
      error = String(e);
    }
  }

  async function closeTab(i: number) {
    const t = tabs[i];
    if (!t) return;
    if (t.alive) {
      try { await invoke('disconnect', { id: t.sid }); } catch { /* 已断开 */ }
    }
    fitFns.delete(t.sid);
    tabApis.delete(t.sid);
    tabs.splice(i, 1);
    tabs = tabs;
    if (activeIdx >= tabs.length) activeIdx = tabs.length - 1;
    const nt = tabs[activeIdx];
    if (nt) requestAnimationFrame(() => fitFns.get(nt.sid)?.());
  }

  function onTabClosed(sid: string) {
    const t = tabs.find((x) => x.sid === sid);
    if (t) t.alive = false;
  }

  function doReconnect() {
    const t = activeTab;
    if (!t) return;
    const i = activeIdx;
    closeTab(i);
    connect(t.profile);
  }

  function setFont(size: number) {
    fontSize = size === 999 ? 14 : Math.min(28, Math.max(10, size));
    localStorage.setItem('fontSize', String(fontSize));
    const t = activeTab;
    if (t) requestAnimationFrame(() => fitFns.get(t.sid)?.());
  }

  function onZoom(e: Event) {
    const d = (e as CustomEvent<number>).detail;
    setFont(d === 999 ? 999 : fontSize + d);
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (e.ctrlKey && e.key === 'Tab') {
      e.preventDefault();
      if (tabs.length > 1) {
        activeIdx = (activeIdx + (e.shiftKey ? -1 : 1) + tabs.length) % tabs.length;
        const t = activeTab;
        if (t) requestAnimationFrame(() => fitFns.get(t.sid)?.());
      }
    } else if (e.ctrlKey && e.shiftKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      if (tabs.length > 1) {
        activeIdx = (activeIdx + (e.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
      }
    } else if (e.ctrlKey && e.key.toLowerCase() === 'b') {
      e.preventDefault();
      showAside = !showAside;
      localStorage.setItem('asideHidden', showAside ? '0' : '1');
    } else if (e.ctrlKey && e.key.toLowerCase() === 'w') {
      if (activeIdx >= 0) { e.preventDefault(); closeTab(activeIdx); }
    }
  }

  onMount(async () => {
    window.addEventListener('resize', onWinResize);
    window.addEventListener('lterm-zoom', onZoom);
    window.addEventListener('keydown', onWindowKeydown);
    unlistenAll.push(await listen<{ host: string; port: number; fingerprint: string }>('hostkey-new', (ev) => {
      noticeQueue.push(`\x1b[33m[安全] 新主机 ${ev.payload.host}:${ev.payload.port}，指纹已记录: ${ev.payload.fingerprint}\x1b[0m`);
    }));
    unlistenAll.push(await listen<{ host: string; port: number; fingerprint: string }>('hostkey-mismatch', (ev) => {
      const msg = `主机密钥与记录不一致 ${ev.payload.host}:${ev.payload.port} (${ev.payload.fingerprint})，连接已拒绝`;
      error = msg;
    }));
    await refresh();
  });

  function flushNotices() {
    requestAnimationFrame(() => {
      const t = tabs[activeIdx];
      if (!t) return;
      while (noticeQueue.length) tabApis.get(t.sid)?.writeLine(noticeQueue.shift()!);
    });
  }

  function onWinResize() {
    const t = activeTab;
    if (t) requestAnimationFrame(() => fitFns.get(t.sid)?.());
  }

  $effect(() => {
    localStorage.setItem('prefCopyOnSelect', prefCopyOnSelect ? '1' : '0');
    localStorage.setItem('prefConfirmMultiLine', prefConfirmMultiLine ? '1' : '0');
  });

  onDestroy(() => {
    window.removeEventListener('resize', onWinResize);
    window.removeEventListener('lterm-zoom', onZoom);
    window.removeEventListener('keydown', onWindowKeydown);
    for (const u of unlistenAll) u();
  });

  async function openSettings() {
    showSettings = !showSettings;
    if (showSettings && !fontList) {
      try { fontList = await invoke<string[]>('list_fonts'); } catch { fontList = []; }
    }
  }

  function setFontFamily(v: string) {
    fontFamily = v;
    localStorage.setItem('fontFamily', v);
  }

  const authLabel = { password: '密码', key: '密钥', agent: 'agent' } as const;
</script>

<main>
  {#if !showAside}
    <button class="rail" onclick={() => { showAside = true; localStorage.setItem('asideHidden', '0'); }} title="显示服务器列表 (Ctrl+B)">⟩</button>
  {/if}
  <aside style:display={showAside ? 'flex' : 'none'}>
    <div class="toolbar">
      <input class="search" placeholder="模糊搜索：名称 备注 用户 主机…" bind:value={query} oninput={search} />
      <button onclick={() => { showForm = !showForm; }} title="添加服务器">＋</button>
      <button onclick={() => { showAside = false; localStorage.setItem('asideHidden', '1'); }} title="隐藏列表 (Ctrl+B)">⟨</button>
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

  <section class="right">
    <div class="tabbar">
      {#each tabs as t, i}
        <div class="tab" class:active={i === activeIdx} role="tab" tabindex={0} aria-selected={i === activeIdx}
             onclick={() => { activeIdx = i; requestAnimationFrame(() => fitFns.get(t.sid)?.()); }}
             onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activeIdx = i; requestAnimationFrame(() => fitFns.get(t.sid)?.()); } }}>
          <span class="dot" class:connected={t.alive} class:closed={!t.alive}></span>
          {t.profile.name}
          <button class="close" aria-label={`关闭标签 ${t.profile.name}`}
                  onclick={(e) => { e.stopPropagation(); closeTab(i); }}>×</button>
        </div>
      {/each}
      {#if tabs.length}
        <button class="tab newtab" onclick={() => { query = ''; document.querySelector<HTMLInputElement>('.search')?.focus(); }} title="从列表选择服务器新建会话">＋</button>
      {/if}
    </div>

    {#if error}<div class="error">{error} <button class="dismiss" onclick={() => error = ''}>✕</button></div>{/if}

    <div class="panes" bind:this={paneEl}>
      {#if tabs.length === 0}
        <div class="welcome">
          <h2>lterm</h2>
          <p>左侧添加服务器，单击连接。支持 ssh-agent / 密钥 / 密码认证。</p>
          <p class="kbd">Ctrl+Tab 切换标签 · Ctrl+F 搜索 · Ctrl+Shift+C/V 复制/粘贴 · Ctrl+W 关闭标签 · 右键 复制/粘贴</p>
        </div>
      {/if}
      {#each tabs as t, i (t.sid)}
        <Session sessionId={t.sid}
                 profileId={t.profile.id}
                 sessionPassword={t.password}
                 {fontFamily}
                 active={i === activeIdx}
                 {fontSize}
                 {prefCopyOnSelect}
                 {prefConfirmMultiLine}
                 onClosed={() => onTabClosed(t.sid)}
                 onResize={(cols, rows) => { if (t.alive) invoke('resize', { id: t.sid, cols, rows }); }}
                 registerFit={(fn) => fitFns.set(t.sid, fn)}
                 registerApi={(sid, api) => tabApis.set(sid, api)} />
      {/each}
    </div>

    <div class="status">
      {#if activeTab}
        <span class="dot" class:connected={activeTab.alive} class:closed={!activeTab.alive}></span>
        {activeTab.alive ? activeTab.profile.username + '@' + activeTab.profile.host : '会话已结束'}
        {#if activeTab.alive}
          <button onclick={() => closeTab(activeIdx)}>断开</button>
        {:else}
          <button onclick={doReconnect}>重连 {activeTab.profile.name}</button>
        {/if}
      {:else}
        <span class="dot"></span>无活动会话
      {/if}
      <span class="spacer"></span>
      <button class="gear" onclick={openSettings} title="终端设置">⚙</button>
      {#if showSettings}
        <div class="settings">
          <label><input type="checkbox" bind:checked={prefCopyOnSelect} /> 选中即复制</label>
          <label><input type="checkbox" bind:checked={prefConfirmMultiLine} /> 多行粘贴确认</label>
          <div class="row">
            <span>字号 {fontSize}</span>
            <button onclick={() => setFont(fontSize - 1)}>−</button>
            <button onclick={() => setFont(fontSize + 1)}>＋</button>
          </div>
          <div class="fontpick">
            <span>终端字体</span>
            <input list="fontlist" placeholder="默认（留空）" value={fontFamily}
                   aria-label="终端字体"
                   onchange={(e) => setFontFamily((e.target as HTMLInputElement).value)} />
            <datalist id="fontlist">
              {#each (fontList ?? []) as f (f)}<option value={f}></option>{/each}
            </datalist>
            <div class="hint">如 MesloLGS NF / Sarasa Mono SC；直接输入名称亦可，需系统已安装</div>
          </div>
          <div class="hint">右键：复制选区/粘贴 · Ctrl+Shift+C/V · Ctrl+F 搜索 · Ctrl+滚轮 缩放</div>
        </div>
      {/if}
    </div>
  </section>
</main>

<style>
  main { display: flex; height: 100vh; font-family: system-ui, sans-serif; }
  .rail { position: absolute; left: 0; top: 0; bottom: 0; width: 22px; z-index: 30; border: none; background: #181818; color: #8ab4f8; cursor: pointer; }
  .rail:hover { background: #232323; }
  main { position: relative; }
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
  .right { flex: 1; display: flex; flex-direction: column; background: #101010; min-width: 0; }
  .tabbar { display: flex; background: #181818; border-bottom: 1px solid #2a2a2a; overflow-x: auto; }
  .tabbar::-webkit-scrollbar { display: none; }
  .tab { display: flex; align-items: center; gap: 6px; padding: 6px 12px; background: none; border: none; border-right: 1px solid #262626; color: #999; cursor: pointer; font-size: 13px; white-space: nowrap; }
  .tab.active { background: #101010; color: #eee; }
  .tab .close { border: none; background: none; color: inherit; padding: 0 2px; border-radius: 3px; cursor: pointer; line-height: 1; }
  .tab .close:hover { background: #444; color: #fff; }
  .newtab { color: #6a9fb5; }
  .panes { flex: 1; min-height: 0; position: relative; display: flex; }
  .panes > :global(div:not(.welcome)) { position: absolute; inset: 0; }
  .welcome { margin: auto; text-align: center; color: #777; }
  .welcome h2 { color: #aaa; margin-bottom: 4px; }
  .welcome .kbd { font-size: 12px; color: #555; }
  .error { padding: 6px 10px; background: #4a1d1d; color: #f0a0a0; font-size: 13px; display: flex; justify-content: space-between; }
  .dismiss { background: none; border: none; color: #f0a0a0; cursor: pointer; }
  .status { padding: 4px 10px; font-size: 12px; color: #999; border-top: 1px solid #333; display: flex; gap: 10px; align-items: center; position: relative; }
  .status button { padding: 2px 10px; }
  .spacer { flex: 1; }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #555; }
  .dot.connected { background: #4caf50; }
  .dot.closed { background: #e6a23c; }
  .gear { background: none; border: none; color: #999; cursor: pointer; font-size: 14px; }
  .settings { position: absolute; bottom: 28px; right: 8px; background: #252525; border: 1px solid #444; border-radius: 8px; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; z-index: 10; min-width: 230px; color: #ccc; }
  .settings label { display: flex; gap: 6px; align-items: center; cursor: pointer; }
  .settings .row { display: flex; gap: 8px; align-items: center; }
  .settings .hint { font-size: 11px; color: #777; }
  .fontpick { display: flex; flex-direction: column; gap: 4px; }
  .fontpick input { padding: 4px 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
</style>
