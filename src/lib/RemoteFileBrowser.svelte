<script lang="ts">
  import { tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { SvelteSet } from 'svelte/reactivity';
  import FilePreview from './FilePreview.svelte';

  type Props = { sid: string };
  let { sid }: Props = $props();

  type Entry = {
    name: string; path: string; size: number; is_dir: boolean;
    is_link?: boolean | null;
  };
  type Row = {
    path: string; name: string; depth: number; isDir: boolean; isLink: boolean; note?: string;
  };

  const MAX_ROWS_PER_DIR = 3000;
  const MAX_DEPTH = 32;

  let rootEntries = $state.raw<Entry[]>([]);
  let childEntries = $state.raw<Record<string, Entry[]>>({});
  let errors = $state.raw<Record<string, string>>({});
  const expanded = new SvelteSet<string>();
  const loading = new SvelteSet<string>();
  let activePath = $state<string | null>(null);
  let selectedPath = $state<string | null>(null);
  let selectedName = $state('');
  let homePath = $state<string | null>(null);
  let ready = $state(false);
  let fatal = $state('');
  let previewNonce = $state(0);

  function noteRow(parent: string, depth: number, text: string): Row {
    return { path: `${parent}#note`, name: text, depth, isDir: false, isLink: false, note: text };
  }

  function pushNode(en: Entry, depth: number, out: Row[]) {
    out.push({ path: en.path, name: en.name, depth, isDir: en.is_dir, isLink: !!en.is_link });
    if (!en.is_dir || !expanded.has(en.path)) return;
    if (depth >= MAX_DEPTH) {
      out.push(noteRow(en.path, depth + 1, '层级过深，不再展开'));
      return;
    }
    if (loading.has(en.path)) {
      out.push(noteRow(en.path, depth + 1, '加载中…'));
      return;
    }
    const err = errors[en.path];
    if (err) {
      out.push(noteRow(en.path, depth + 1, `加载失败：${err}`));
      return;
    }
    const kids = childEntries[en.path];
    if (!kids) {
      out.push(noteRow(en.path, depth + 1, '加载中…'));
      return;
    }
    if (!kids.length) {
      out.push(noteRow(en.path, depth + 1, '空目录'));
      return;
    }
    for (const k of kids.slice(0, MAX_ROWS_PER_DIR)) pushNode(k, depth + 1, out);
    if (kids.length > MAX_ROWS_PER_DIR) {
      out.push(noteRow(en.path, depth + 1, `仅显示前 ${MAX_ROWS_PER_DIR} 项`));
    }
  }

  const rows = $derived.by(() => {
    const out: Row[] = [{ path: '/', name: '/', depth: 0, isDir: true, isLink: false }];
    if (!rootEntries.length) {
      out.push(noteRow('/', 1, '空目录'));
      return out;
    }
    for (const en of rootEntries.slice(0, MAX_ROWS_PER_DIR)) pushNode(en, 1, out);
    if (rootEntries.length > MAX_ROWS_PER_DIR) {
      out.push(noteRow('/', 1, `仅显示前 ${MAX_ROWS_PER_DIR} 项`));
    }
    return out;
  });

  async function listDir(path: string): Promise<Entry[]> {
    return await invoke<Entry[]>('sftp_list', { sid, path });
  }

  async function scrollToPath(path: string) {
    await tick();
    const el = document.querySelector<HTMLElement>(`[data-path="${CSS.escape(path)}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }

  async function expandTo(home: string) {
    if (!home || home === '/') return;
    const segs = home.split('/').filter(Boolean);
    let cur = '';
    for (const seg of segs) {
      cur += '/' + seg;
      if (cur === home) break;
      if (!childEntries[cur]) {
        try {
          const list = await listDir(cur);
          childEntries = { ...childEntries, [cur]: list };
        } catch {
          return;
        }
      }
      expanded.add(cur);
    }
    activePath = home;
    await scrollToPath(home);
  }

  async function init() {
    fatal = '';
    ready = false;
    try {
      rootEntries = await listDir('/');
      expanded.add('/');
      ready = true;
      let home = '/';
      try {
        home = await invoke<string>('sftp_canonicalize', { sid, path: '.' });
      } catch { /* 拿不到 home 就停在根 */ }
      if (home && home !== '/') {
        homePath = home;
        await expandTo(home);
      }
    } catch (e) {
      fatal = String(e);
    } finally {
      ready = true;
    }
  }
  init();

  async function toggle(path: string) {
    if (path === '/') return;
    if (expanded.has(path) && !errors[path]) {
      expanded.delete(path);
      return;
    }
    expanded.add(path);
    if (childEntries[path]) return;
    loading.add(path);
    if (errors[path]) errors = { ...errors, [path]: '' };
    try {
      const list = await listDir(path);
      childEntries = { ...childEntries, [path]: list };
    } catch (e) {
      errors = { ...errors, [path]: String(e) };
    } finally {
      loading.delete(path);
    }
  }

  function onRow(row: Row) {
    if (row.note) return;
    if (row.path === '/') return;
    activePath = row.path;
    if (row.isDir) {
      void toggle(row.path);
      return;
    }
    selectedPath = row.path;
    selectedName = row.name;
  }

  async function refresh() {
    childEntries = {};
    errors = {};
    loading.clear();
    expanded.clear();
    expanded.add('/');
    activePath = null;
    previewNonce++;
    await init();
  }
</script>

<div class="wrap">
  <aside class="side">
    <div class="side-head">
      <span class="ttl">远程文件</span>
      {#if homePath}
        <button title="定位到主目录" onclick={() => void scrollToPath(homePath ?? '/')}>~</button>
      {/if}
      <button title="刷新" onclick={() => void refresh()}>⟳</button>
    </div>
    {#if fatal}
      <div class="side-err">
        <div>{fatal}</div>
        <button onclick={() => void refresh()}>重试</button>
      </div>
    {:else if !ready}
      <div class="side-msg">加载中…</div>
    {:else}
      <div class="tree" role="tree" aria-label="远端文件树">
        {#each rows as row}
          {#if row.note}
            <div class="row note" style:padding-left={`${8 + row.depth * 14}px`}>{row.name}</div>
          {:else}
            <button type="button" role="treeitem" aria-level={row.depth + 1}
                    aria-selected={row.path === activePath}
                    aria-expanded={row.isDir ? expanded.has(row.path) : undefined}
                    class="row" class:dir={row.isDir} class:active={row.path === activePath}
                    class:root={row.path === '/'}
                    data-path={row.path}
                    title={row.path}
                    onclick={() => onRow(row)}>
              <span class="tw">{row.isDir && row.path !== '/' ? (expanded.has(row.path) ? '▾' : '▸') : ''}</span>
              <span class="ic">{row.isDir ? '📁' : '📄'}</span>
              <span class="nm">{row.name}</span>
              {#if row.isLink}<span class="lk">↗</span>{/if}
            </button>
          {/if}
        {/each}
      </div>
    {/if}
  </aside>
  <main class="main">
    <FilePreview {sid} path={selectedPath ?? ''} name={selectedName} nonce={previewNonce} />
  </main>
</div>

<style>
  .wrap { display: flex; height: 100vh; background: #1e1e1e; color: #ddd; font-family: system-ui, sans-serif; font-size: 13px; }
  .side { width: 300px; min-width: 220px; flex-shrink: 0; display: flex; flex-direction: column; background: #161616; border-right: 1px solid #333; }
  .side-head { display: flex; align-items: center; gap: 6px; padding: 6px 10px; border-bottom: 1px solid #2a2a2a; }
  .side-head .ttl { flex: 1; color: #999; font-size: 12px; }
  .side-head button { background: none; border: none; color: #8ab4f8; cursor: pointer; font-size: 13px; padding: 2px 6px; border-radius: 4px; }
  .side-head button:hover { background: #232323; }
  .side-msg { padding: 14px; color: #777; }
  .side-err { padding: 14px; color: #e57373; display: flex; flex-direction: column; gap: 10px; }
  .side-err button { align-self: flex-start; background: #232323; border: 1px solid #3a3a3a; color: #ccc; border-radius: 5px; padding: 3px 10px; cursor: pointer; }
  .tree { flex: 1; overflow: auto; padding: 4px 0 12px; }
  .row { display: flex; align-items: center; gap: 4px; width: calc(100% - 8px); margin: 0 4px; padding: 2px 6px;
    background: none; border: none; color: #ccc; font: inherit; text-align: left; cursor: pointer; border-radius: 4px; white-space: nowrap; }
  .row:hover { background: #232323; }
  .row.active { background: #22304a; color: #cfe1ff; }
  .row.root { color: #999; }
  .row.note { color: #777; font-style: italic; cursor: default; }
  .row.note:hover { background: none; }
  .tw { width: 12px; flex-shrink: 0; color: #888; }
  .ic { width: 16px; flex-shrink: 0; }
  .nm { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .lk { color: #8ab4f8; font-size: 11px; flex-shrink: 0; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
</style>