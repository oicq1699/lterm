<script lang="ts">
  import { onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { hopPromptLabel, type ChainHop } from '$lib/chainPrompt';

  type Props = {
    sid: string; profileId: string; password: string | null; wide?: boolean; histMax?: number;
    /** 删除前弹确认（多选只问一次） */
    confirmDelete?: boolean;
    /** 传输遇同名文件弹确认；关掉就是静默覆盖 */
    confirmOverwrite?: boolean;
  };
  let { sid, profileId, password, wide = false, histMax = 8, confirmDelete = true, confirmOverwrite = false }: Props = $props();

  type Entry = {
    name: string; path: string; size: number; is_dir: boolean; modified: number;
    /** 自身是软链接；is_dir 已反映链接指向的目标 */
    is_link?: boolean | null;
    mode?: number | null; uid?: number | null; gid?: number | null;
    user?: string | null; group?: string | null;
  };
  type Transfer = {
    id: string; label: string; dir: 'up' | 'down'; done: number; total: number; state: 'run' | 'ok' | 'err'; error?: string;
    /** 发起传输时两侧所在目录，成功后才进历史 */
    fromLocal: string; fromRemote: string;
  };
  type DirState = { local: string | null; remote: string | null; localHist: string[]; remoteHist: string[] };

  const cap = () => Math.min(30, Math.max(1, Math.round(histMax) || 8));
  const withHist = (list: string[], path: string) => [path, ...list.filter((p) => p !== path)].slice(0, cap());

  let ready = $state(false);
  let error = $state('');
  let localPath = $state('');
  let remotePath = $state('.');
  let localEntries = $state<Entry[]>([]);
  let remoteEntries = $state<Entry[]>([]);
  let localSel = $state<Set<string>>(new Set());
  let remoteSel = $state<Set<string>>(new Set());
  let localAnchor = $state<string | null>(null);
  let remoteAnchor = $state<string | null>(null);
  let transfers = $state<Transfer[]>([]);
  let localHist = $state<string[]>([]);
  let remoteHist = $state<string[]>([]);
  let openHist = $state<'' | 'local' | 'remote'>('');
  let unlisten: UnlistenFn[] = [];

  const fmtSize = (n: number) => n >= 1073741824 ? (n / 1073741824).toFixed(2) + 'G'
    : n >= 1048576 ? (n / 1048576).toFixed(1) + 'M'
    : n >= 1024 ? (n / 1024).toFixed(0) + 'K' : String(n);
  const fmtDate = (t: number) => t ? new Date(t * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '';

  // ls 风格的 rwx 九位；setuid/sticky 等特定位在 title 的八进制里体现
  const fmtRwx = (mode: number | null | undefined, kind: string) => {
    if (mode == null) return '';
    const bits = 'rwxrwxrwx';
    let s = '';
    for (let i = 0; i < 9; i++) s += (mode >> (8 - i)) & 1 ? bits[i] : '-';
    return kind + s;
  };
  const kindOf = (en: Entry) => en.is_link ? 'l' : en.is_dir ? 'd' : '-';
  const iconOf = (en: Entry) => en.is_dir ? '📁' : '📄';
  const linkTip = (en: Entry) => '软链接' + (en.is_dir ? '（指向目录）' : '（目标类型未能确认，双击会先按目录尝试）');
  const fmtOctal = (mode: number | null | undefined) => (mode == null ? '' : '0' + (mode & 0o7777).toString(8).padStart(3, '0'));
  const fmtOwner = (en: Entry) => en.user ?? (en.uid != null ? String(en.uid) : '');
  const fmtGroup = (en: Entry) => en.group ?? (en.gid != null ? String(en.gid) : '');

  function localSep(p: string) { return /^[a-zA-Z]:|\\/.test(p) ? '\\' : '/'; }
  function joinLocal(dir: string, name: string) {
    const sep = localSep(dir);
    return dir.endsWith(sep) ? dir + name : dir + sep + name;
  }
  function joinPosix(dir: string, name: string) {
    return dir.endsWith('/') ? dir + name : dir + '/' + name;
  }
  function parentLocal(p: string) {
    const sep = localSep(p);
    const trimmed = p.replace(/[\\/]+$/, '');
    const i = trimmed.lastIndexOf(sep);
    if (i <= 0) return trimmed + sep;
    return trimmed.slice(0, i + 1);
  }
  function parentPosix(p: string) {
    const t = p.replace(/\/+$/, '');
    const i = t.lastIndexOf('/');
    return i <= 0 ? '/' : t.slice(0, i + 1);
  }
  function base(p: string) { return p.split(/[\\/]/).filter(Boolean).pop() ?? p; }

  async function ensureOpen() {
    if (ready) return;
    const chain = await invoke<ChainHop[]>('get_proxy_chain', { profileId });
    const pwMap: Record<string, string> = {};
    if (password) pwMap[profileId] = password;
    for (const h of chain) {
      if (h.id === profileId && password) continue;
      const label = hopPromptLabel(h);
      if (!label) continue;
      const pw = prompt(label);
      if (pw === null) throw new Error('已取消');
      pwMap[h.id] = pw;
    }
    await invoke('sftp_open', { sid, profileId, password, proxyPasswords: pwMap });
    ready = true;
  }

  function saveState() {
    invoke('set_dir_state', { profileId, local: localPath || null, remote: remotePath || null }).catch(() => {});
  }

  async function loadLocal(path?: string, quiet = false): Promise<boolean> {
    const p = path ?? localPath;
    try {
      const r = await invoke<{ name: string; path: string; size: number; is_dir: boolean; modified: number }[]>('local_list', { path: p });
      localPath = p;
      localEntries = r as Entry[];
      localSel = new Set();
      localAnchor = null;
      saveState();
      return true;
    } catch (e) { if (!quiet) error = String(e); return false; }
  }

  async function loadRemote(path?: string, quiet = false): Promise<boolean> {
    const p = path ?? remotePath;
    try {
      const r = await invoke<Entry[]>('sftp_list', { sid, path: p });
      remotePath = p;
      remoteEntries = r;
      remoteSel = new Set();
      remoteAnchor = null;
      saveState();
      return true;
    } catch (e) { if (!quiet) error = String(e); return false; }
  }

  async function init() {
    error = '';
    try {
      await ensureOpen();
      const st = await invoke<DirState>('get_dir_state', { profileId });
      localHist = st.localHist ?? [];
      remoteHist = st.remoteHist ?? [];
      if (!(st.local && await loadLocal(st.local, true))) {
        await loadLocal(await invoke<string>('local_home'));
      }
      let startOk = false;
      if (st.remote) startOk = await loadRemote(st.remote, true);
      if (!startOk) {
        startOk = await loadRemote('.', true);
        if (startOk) {
          try { remotePath = await invoke<string>('sftp_canonicalize', { sid, path: remotePath }); } catch { /* 保留原路径 */ }
        }
      }
    } catch (e) { error = String(e); }
  }
  init();

  listen<{ id: string; done: number; total: number }>('sftp-progress', (ev) => {
    const t = transfers.find((x) => x.id === ev.payload.id);
    if (t) { t.done = ev.payload.done; t.total = ev.payload.total; }
  }).then((u) => unlisten.push(u));
  listen<{ id: string; ok: boolean; error: string }>('sftp-done', (ev) => {
    const t = transfers.find((x) => x.id === ev.payload.id);
    if (t) {
      t.state = ev.payload.ok ? 'ok' : 'err';
      t.error = ev.payload.error;
      if (ev.payload.ok) {
        // 传成功才算历史目录，浏览路过不算
        localHist = withHist(localHist, t.fromLocal);
        remoteHist = withHist(remoteHist, t.fromRemote);
        invoke('note_transfer_dirs', { profileId, local: t.fromLocal, remote: t.fromRemote, keep: cap() }).catch(() => {});
        refreshAfter(t);
      }
    }
  }).then((u) => unlisten.push(u));

  // 传输成功后刷「目标那一端」：上传刷远端、下载刷本地，省得手工点 ⟳
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  function refreshAfter(t: Transfer) {
    clearTimeout(refreshTimer);
    // 批量传输合并成一次刷新
    refreshTimer = setTimeout(() => {
      // 只在用户仍停在发起传输的那个目录时刷，否则会把人从别处拽回去
      if (t.dir === 'up' && remotePath === t.fromRemote) void loadRemote(undefined, true);
      else if (t.dir === 'down' && localPath === t.fromLocal) void loadLocal(undefined, true);
    }, 300);
  }

  function pickHist(which: 'local' | 'remote', path: string) {
    openHist = '';
    which === 'local' ? loadLocal(path) : loadRemote(path);
  }

  const entriesOf = (which: 'local' | 'remote') => (which === 'local' ? localEntries : remoteEntries);

  function setSel(which: 'local' | 'remote', next: Set<string>, anchor: string | null) {
    if (which === 'local') { localSel = next; localAnchor = anchor; }
    else { remoteSel = next; remoteAnchor = anchor; }
  }

  function toggleSel(which: 'local' | 'remote', path: string, e: MouseEvent) {
    const sel = which === 'local' ? localSel : remoteSel;
    const anchor = which === 'local' ? localAnchor : remoteAnchor;
    const paths = entriesOf(which).map((x) => x.path);
    const a = anchor ? paths.indexOf(anchor) : -1;
    if (e.shiftKey && a >= 0) {
      // Shift 是「从锚点拉到这一行」：整段替换当前选择，锚点不动，方便接着往下拉
      const b = paths.indexOf(path);
      setSel(which, new Set(paths.slice(Math.min(a, b), Math.max(a, b) + 1)), anchor);
      return;
    }
    const next = new Set(sel);
    if (e.ctrlKey || e.metaKey) {
      if (next.has(path)) next.delete(path); else next.add(path);
    } else {
      next.clear(); next.add(path);
    }
    setSel(which, next, path);
  }

  function selectAll(which: 'local' | 'remote') {
    const paths = entriesOf(which).map((x) => x.path);
    setSel(which, new Set(paths), paths[0] ?? null);
  }

  // Ctrl+A 只挂在列表子树上：终端里的 Ctrl+A 由 xterm 自己的输入区收，
  // 路径框里的 Ctrl+A 仍是浏览器原生的全选文字
  function listKey(e: KeyboardEvent, which: 'local' | 'remote') {
    if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== 'a') return;
    if ((e.target as HTMLElement | null)?.tagName === 'INPUT') return;
    e.preventDefault();
    selectAll(which);
  }

  function entryDbl(which: 'local' | 'remote', en: Entry) {
    if (en.is_dir) {
      which === 'local' ? loadLocal(en.path) : loadRemote(en.path);
    } else if (which === 'local') {
      uploadSelected();
    } else if (en.is_link) {
      // 列表里没能解析出链接目标类型时（老服务端 / 目录权限受限），先按目录进，
      // 进不去再按文件下载，两头都不会卡住
      void loadRemote(en.path, true).then((ok) => {
        if (ok) return;
        remoteSel = new Set([en.path]);
        remoteAnchor = en.path;
        downloadSelected();
      });
    } else {
      downloadSelected();
    }
  }

  function newTransfer(id: string, dir: 'up' | 'down'): Transfer {
    const t: Transfer = { id, label: base(id), dir, done: 0, total: 0, state: 'run', fromLocal: localPath, fromRemote: remotePath };
    transfers = [...transfers.filter((x) => x.id !== id), t];
    return t;
  }

  // 后端遇到同名文件是直接截断覆盖的，所以起传输前拿对面那份已加载的列表比一次名字
  function askOverwrite(dir: 'up' | 'down', names: string[]): boolean {
    if (!confirmOverwrite) return true;
    const exist = dir === 'up' ? remoteEntries : localEntries;
    const hit = names.filter((n) => exist.some((e) => e.name === n));
    if (!hit.length) return true;
    return confirm(`${dir === 'up' ? '上传' : '下载'}会覆盖同名 ${hit.length} 项：${hit.slice(0, 5).join('、')}${hit.length > 5 ? '…' : ''}。要继续吗？`);
  }

  async function uploadSelected() {
    error = '';
    const items = [...localSel].map((p) => localEntries.find((x) => x.path === p)).filter((x): x is Entry => !!x);
    if (!items.length || !askOverwrite('up', items.map((e) => e.name))) return;
    try { await ensureOpen(); } catch (e) { error = String(e); return; }
    for (const en of items) {
      const target = joinPosix(remotePath, en.name);
      newTransfer(target, 'up');
      invoke('sftp_transfer', { req: { sid, transfer_id: target, from: en.path, to: target }, download: false })
        .catch((e) => { const t = transfers.find((x) => x.id === target); if (t) { t.state = 'err'; t.error = String(e); } });
    }
  }

  async function downloadSelected() {
    error = '';
    const items = [...remoteSel].map((p) => remoteEntries.find((x) => x.path === p)).filter((x): x is Entry => !!x);
    if (!items.length || !askOverwrite('down', items.map((e) => e.name))) return;
    try { await ensureOpen(); } catch (e) { error = String(e); return; }
    for (const en of items) {
      const target = joinLocal(localPath, en.name);
      newTransfer(target, 'down');
      invoke('sftp_transfer', { req: { sid, transfer_id: target, from: en.path, to: target }, download: true })
        .catch((e) => { const t = transfers.find((x) => x.id === target); if (t) { t.state = 'err'; t.error = String(e); } });
    }
  }

  async function cancelTransfer(t: Transfer) {
    try { await invoke('sftp_cancel', { sid, transferId: t.id }); } catch (e) { error = String(e); }
  }

  function dirNamePrompt(): string | null {
    const name = prompt('新建目录名：')?.trim();
    if (!name) return null;
    if (name === '.' || name === '..' || /[\\/]/.test(name)) {
      error = '目录名不能包含路径分隔符，也不能是 . 或 ..';
      return null;
    }
    return name;
  }

  async function mkdirRemote() {
    const name = dirNamePrompt();
    if (!name) return;
    try {
      await invoke('sftp_mkdir', { sid, path: joinPosix(remotePath, name) });
      await loadRemote();
    } catch (e) { error = String(e); }
  }

  async function mkdirLocal() {
    const name = dirNamePrompt();
    if (!name) return;
    try {
      await invoke('local_mkdir', { path: joinLocal(localPath, name) });
      await loadLocal();
    } catch (e) { error = String(e); }
  }

  async function del(which: 'local' | 'remote') {
    const sel = which === 'local' ? localSel : remoteSel;
    const entries = entriesOf(which);
    const items = [...sel].map((p) => entries.find((x) => x.path === p)).filter((x): x is Entry => !!x);
    if (!items.length) return;
    if (confirmDelete) {
      // 多选只问一次，所以要把「有多少项、里面有目录」讲清楚
      const dirs = items.filter((e) => e.is_dir).length;
      const names = items.slice(0, 5).map((e) => e.name).join('、');
      const more = items.length > 5 ? `… 共 ${items.length} 项` : '';
      if (!confirm(`删除 ${names}${more}${dirs ? `（其中 ${dirs} 个目录连内容一起删）` : ''}？`)) return;
    }
    for (const en of items) {
      try {
        if (which === 'remote') await invoke('sftp_remove', { sid, path: en.path, recursive: en.is_dir });
        else await invoke('local_remove', { path: en.path, recursive: en.is_dir });
      } catch (e) { error = String(e); }
    }
    which === 'local' ? await loadLocal() : await loadRemote();
  }

  async function rename(which: 'local' | 'remote') {
    const sel = which === 'local' ? localSel : remoteSel;
    if (sel.size !== 1) { error = '请选择一个条目重命名'; return; }
    const p = [...sel][0];
    const name = prompt('新名称：', base(p)) as string | null;
    if (!name) return;
    try {
      if (which === 'remote') await invoke('sftp_rename', { sid, from: p, to: joinPosix(parentPosix(p), name) });
      else await invoke('local_rename', { from: p, to: joinLocal(parentLocal(p), name) });
      which === 'local' ? await loadLocal() : await loadRemote();
    } catch (e) { error = String(e); }
  }

  // 拖拽上传：Tauri 窗口级事件提供 native 文件路径
  import { getCurrentWindow } from '@tauri-apps/api/window';
  getCurrentWindow().onDragDropEvent((ev) => {
    if (ev.payload.type === 'drop') {
      if (!askOverwrite('up', ev.payload.paths.map((p) => base(p)))) return;
      for (const path of ev.payload.paths) {
        const name = base(path);
        const target = joinPosix(remotePath, name);
        newTransfer(target, 'up');
        ensureOpen()
          .then(() => invoke('sftp_transfer', { req: { sid, transfer_id: target, from: path, to: target }, download: false }))
          .catch((e) => { const t = transfers.find((x) => x.id === target); if (t) { t.state = 'err'; t.error = String(e); } });
      }
    }
  }).then((u) => unlisten.push(u));

  onDestroy(() => {
    clearTimeout(refreshTimer);
    for (const u of unlisten) u();
  });
</script>

<div class="browser">
  {#if error}
    <div class="err">{error} <button onclick={() => error = ''} aria-label="关闭提示">✕</button></div>
  {/if}
  <div class="panes">
    <div class="pane">
      <div class="pathbar">
        <span class="tag">本地</span>
        <button onclick={() => loadLocal(parentLocal(localPath))} title="上一级">↑</button>
        <input class="path" value={localPath} aria-label="本地路径"
               onkeydown={(e) => e.key === 'Enter' && loadLocal((e.target as HTMLInputElement).value)} />
        <button onclick={() => loadLocal()} title="刷新">⟳</button>
        <button class="hbtn" disabled={!localHist.length} title="历史目录（传过的）" aria-label="本地历史目录"
                onclick={() => openHist = openHist === 'local' ? '' : 'local'}>▾</button>
        {#if openHist === 'local'}
          <div class="histscrim" role="presentation" onclick={() => openHist = ''}></div>
          <ul class="histpop" role="menu" aria-label="本地历史目录">
            {#each localHist as p (p)}
              <li role="menuitem" tabindex={0} onclick={() => pickHist('local', p)}
                  onkeydown={(e) => e.key === 'Enter' && pickHist('local', p)} title={p}>{p}</li>
            {/each}
          </ul>
        {/if}
      </div>
      {#if wide}
        <div class="colhead"><span class="ico"></span><span class="fperm">权限</span><span class="fown">所有者</span><span class="ch-name">名称</span><span class="fsize">大小</span><span class="fdate">时间</span></div>
      {/if}
      <ul role="listbox" aria-label="本地文件列表" onkeydown={(e) => listKey(e, 'local')}>
        {#each localEntries as en (en.path)}
          <li class:selected={localSel.has(en.path)} role="option" aria-selected={localSel.has(en.path)} tabindex={0}
              onclick={(e) => toggleSel('local', en.path, e)}
              onkeydown={(e) => e.key === 'Enter' && entryDbl('local', en)}
              ondblclick={() => entryDbl('local', en)}>
            <span class="ico" class:link={en.is_link} title={en.is_link ? linkTip(en) : undefined}>{iconOf(en)}</span>
            {#if wide}
              <span class="fperm" title={fmtOctal(en.mode)}>{fmtRwx(en.mode, kindOf(en))}</span>
              <span class="fown" title={`${fmtOwner(en)}:${fmtGroup(en)}`}>{fmtOwner(en)}</span>
            {/if}
            <span class="fname" title={en.path}>{en.name}</span>
            <span class="fsize">{en.is_dir ? '' : fmtSize(en.size)}</span>
            <span class="fdate">{fmtDate(en.modified)}</span>
          </li>
        {/each}
      </ul>
      <div class="ops">
        <button onclick={uploadSelected} title="上传选中到远端当前目录（Ctrl+A 全选本目录）">上传 →</button>
        <button onclick={mkdirLocal}>新建目录</button>
        <button onclick={() => del('local')} title="删除选中项（Ctrl+A 全选本目录）">删除</button>
        <button onclick={() => rename('local')}>重命名</button>
      </div>
    </div>
    <div class="pane">
      <div class="pathbar">
        <span class="tag">远端</span>
        <button onclick={() => loadRemote(parentPosix(remotePath))} title="上一级">↑</button>
        <input class="path" value={remotePath} aria-label="远端路径"
               onkeydown={(e) => e.key === 'Enter' && loadRemote((e.target as HTMLInputElement).value)} />
        <button onclick={() => loadRemote()} title="刷新">⟳</button>
        <button class="hbtn" disabled={!remoteHist.length} title="历史目录（传过的）" aria-label="远端历史目录"
                onclick={() => openHist = openHist === 'remote' ? '' : 'remote'}>▾</button>
        {#if openHist === 'remote'}
          <div class="histscrim" role="presentation" onclick={() => openHist = ''}></div>
          <ul class="histpop" role="menu" aria-label="远端历史目录">
            {#each remoteHist as p (p)}
              <li role="menuitem" tabindex={0} onclick={() => pickHist('remote', p)}
                  onkeydown={(e) => e.key === 'Enter' && pickHist('remote', p)} title={p}>{p}</li>
            {/each}
          </ul>
        {/if}
      </div>
      {#if wide}
        <div class="colhead"><span class="ico"></span><span class="fperm">权限</span><span class="fown">所有者</span><span class="ch-name">名称</span><span class="fsize">大小</span><span class="fdate">时间</span></div>
      {/if}
      <ul role="listbox" aria-label="远端文件列表" onkeydown={(e) => listKey(e, 'remote')}>
        {#each remoteEntries as en (en.path)}
          <li class:selected={remoteSel.has(en.path)} role="option" aria-selected={remoteSel.has(en.path)} tabindex={0}
              onclick={(e) => toggleSel('remote', en.path, e)}
              onkeydown={(e) => e.key === 'Enter' && entryDbl('remote', en)}
              ondblclick={() => entryDbl('remote', en)}>
            <span class="ico" class:link={en.is_link} title={en.is_link ? linkTip(en) : undefined}>{iconOf(en)}</span>
            {#if wide}
              <span class="fperm" title={fmtOctal(en.mode)}>{fmtRwx(en.mode, kindOf(en))}</span>
              <span class="fown" title={`${fmtOwner(en)}:${fmtGroup(en)}`}>{fmtOwner(en)}</span>
            {/if}
            <span class="fname" title={en.path}>{en.name}</span>
            <span class="fsize">{en.is_dir ? '' : fmtSize(en.size)}</span>
            <span class="fdate">{fmtDate(en.modified)}</span>
          </li>
        {:else}
          <li class="empty">…</li>
        {/each}
      </ul>
      <div class="ops">
        <button onclick={downloadSelected} title="下载选中到本地当前目录（Ctrl+A 全选本目录）">← 下载</button>
        <button onclick={mkdirRemote}>新建目录</button>
        <button onclick={() => del('remote')} title="删除选中项（Ctrl+A 全选本目录）">删除</button>
        <button onclick={() => rename('remote')}>重命名</button>
      </div>
    </div>
  </div>
  {#if transfers.length}
    <div class="transfers">
      {#each transfers as t (t.id)}
        <div class="tr">
          <span class="dir">{t.dir === 'up' ? '↑' : '↓'}</span>
          <span class="label" title={t.id}>{t.label}</span>
          {#if t.state === 'run'}
            <div class="bar"><div class="fill" style:width={t.total ? (100 * t.done / t.total).toFixed(1) + '%' : '0%'}></div></div>
            <span class="pct">{t.total ? Math.round(100 * t.done / t.total) + '%' : fmtSize(t.done)}</span>
            <button onclick={() => cancelTransfer(t)}>取消</button>
          {:else if t.state === 'ok'}
            <span class="ok">✓ 完成</span>
            <button onclick={() => transfers = transfers.filter((x) => x !== t)}>✕</button>
          {:else}
            <span class="fail" title={t.error}>✗ {t.error || '失败'}</span>
            <button onclick={() => transfers = transfers.filter((x) => x !== t)}>✕</button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .browser { flex: 1; display: flex; flex-direction: column; min-height: 0; color: #ccc; font-size: 12px; }
  .err { padding: 4px 8px; background: #4a1d1d; color: #f0a0a0; display: flex; justify-content: space-between; }
  .err button { background: none; border: none; color: #f0a0a0; cursor: pointer; }
  .panes { flex: 1; display: flex; min-height: 0; }
  .pane { flex: 1; display: flex; flex-direction: column; min-width: 0; border-right: 1px solid #2a2a2a; }
  .pane:last-child { border-right: none; }
  .pathbar { display: flex; gap: 3px; padding: 4px; border-bottom: 1px solid #2a2a2a; align-items: center; position: relative; }
  .tag { color: #8ab4f8; font-size: 11px; }
  .path { flex: 1; min-width: 0; padding: 3px 6px; border-radius: 4px; border: 1px solid #444; background: #222; color: #ddd; font-size: 11px; }
  .pathbar button, .ops button { padding: 2px 7px; font-size: 11px; border-radius: 4px; border: 1px solid #444; background: #2a2a2a; color: #ccc; cursor: pointer; }
  .pane ul { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; }
  .pane li { display: flex; gap: 6px; padding: 2px 8px; cursor: pointer; align-items: baseline; }
  .pane li:hover { background: #232323; }
  .pane li.selected { background: #2d4f60; }
  .pane li:focus-visible { outline: 2px solid #8ab4f8; outline-offset: -2px; }
  .ico { width: 14px; flex-shrink: 0; }
  /* 软链接：不换图标，在类型图标右下角叠一个小箭头角标，目录/文件都适用 */
  .ico.link { position: relative; }
  .ico.link::after { content: '↗\00FE0E'; position: absolute; right: -2px; bottom: -2px;
    font-size: 9px; line-height: 1; color: #8ab4f8; }
  .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fsize { width: 52px; flex-shrink: 0; text-align: right; color: #888; }
  .fperm { width: 78px; color: #86b386; font-family: monospace; flex-shrink: 0; }
  .fown { width: 88px; color: #8a93a6; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex-shrink: 0; }
  .colhead { display: flex; gap: 6px; padding: 2px 8px; color: #666; font-size: 11px; border-bottom: 1px solid #262626; }
  .colhead span { flex-shrink: 0; }
  .colhead .ch-name { flex: 1; }
  .fdate { width: 76px; flex-shrink: 0; text-align: right; color: #666; }
  .empty { color: #666; padding: 8px; }
  .ops { display: flex; gap: 4px; padding: 4px; border-top: 1px solid #2a2a2a; flex-wrap: wrap; }
  .transfers { border-top: 1px solid #333; max-height: 130px; overflow-y: auto; padding: 4px; }
  .tr { display: flex; gap: 6px; align-items: center; padding: 2px 4px; }
  .dir { color: #8ab4f8; }
  .label { flex: 0 0 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bar { flex: 1; height: 8px; background: #2a2a2a; border-radius: 4px; overflow: hidden; }
  .fill { height: 100%; background: #4c8bf5; }
  .pct { width: 40px; text-align: right; }
  .ok { color: #7cc47f; }
  .fail { color: #e57373; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
  .tr button { padding: 1px 8px; font-size: 11px; }
  /* 历史目录浮层：.pane ul / .pathbar button 的优先级更高，这里必须带上父级选择器 */
  .pathbar .hbtn { padding: 2px 6px; }
  .pathbar .hbtn:disabled { color: #555; cursor: default; }
  .histscrim { position: fixed; inset: 0; z-index: 30; }
  .pathbar .histpop { position: absolute; top: 30px; left: 4px; right: 4px; z-index: 31; margin: 0; padding: 2px;
    list-style: none; background: #232323; border: 1px solid #4a4a4a; border-radius: 6px;
    box-shadow: 0 8px 20px rgba(0, 0, 0, .5); max-height: 190px; overflow-y: auto; }
  .pathbar .histpop li { display: block; padding: 4px 8px; font-size: 11px; color: #ccc; cursor: pointer;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .pathbar .histpop li:hover, .pathbar .histpop li:focus-visible { background: #2d4f60; outline: none; }
</style>
