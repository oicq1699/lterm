<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { check as checkAutoUpdate } from '@tauri-apps/plugin-updater';
  import { relaunch } from '@tauri-apps/plugin-process';
  import Session from '$lib/Session.svelte';
  import Icon from '$lib/Icon.svelte';
  import { ICON_NAMES, normalizeIcon, iconPaths } from '$lib/icons';
  import { PALETTE_LABELS } from '$lib/termPalette';
  import { hopPromptLabel, type ChainHop } from '$lib/chainPrompt';

  type RdpOptions = {
    sec: 'auto' | 'nla' | 'weak' | 'tls' | 'legacy';
    shareDir: string | null; clipboard: boolean; fullscreen: boolean; ignoreCert: boolean;
    width: number | null; height: number | null;
  };
  type Profile = {
    id: string; name: string; host: string; port: number; username: string;
    auth_method: 'password' | 'key' | 'agent'; protocol: 'ssh' | 'rdp'; rdp: RdpOptions;
    icon: string | null; key_path: string | null;
    remark: string; group_tag: string | null;
    proxy_jump: string | null;
    created_at: number; updated_at: number;
  };
  type Hit = { server: Profile; score: number };
  type Tab = { sid: string; profile: Profile; alive: boolean; password: string | null; passwords: Record<string, string> };

  let servers: Profile[] = $state([]);
  let folders: string[] = $state([]);
  // 列表被手动排过序：显示按 servers 数组顺序走，新增落到同组末尾
  let manualOrdered = $state(false);
  let hits: Hit[] = $state([]);
  let query = $state('');
  let showForm = $state(false);
  let showIconPicker = $state(false);
  let draft: Profile = $state(emptyDraft());
  let draftSecret = $state('');
  let draftSavePw = $state(false);
  let draftClearPw = $state(false);
  let error = $state('');

  let tabs = $state<Tab[]>([]);
  let activeIdx = $state(-1);
  // 双击到连上之间（TCP+认证最坏可达几十秒）必须有显性反馈：id → 名称
  let connecting = $state<Map<string, string>>(new Map());
  const connectingIds = $derived(new Set(connecting.keys()));
  let connectingMsg = $derived(
    connecting.size === 1 ? `正在连接 ${[...connecting.values()][0]}…`
      : connecting.size > 1 ? `正在连接 ${[...connecting.values()][0]}… 等 ${connecting.size} 台`
      : '');
  function markConnecting(p: Profile) {
    const m = new Map(connecting);
    m.set(p.id, p.name);
    connecting = m;
  }
  function stopConnecting(id: string) {
    if (!connecting.has(id)) return;
    const m = new Map(connecting);
    m.delete(id);
    connecting = m;
  }
  let paneEl: HTMLDivElement | undefined = $state();
  let fitFns = new Map<string, () => void>();
  let tabApis = new Map<string, { writeLine: (t: string) => void }>();
  let noticeQueue: string[] = [];
  // RDP 由外部窗口承载，没有终端可写，结果用状态栏短暂提示
  let flash = $state('');
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  function showFlash(msg: string) {
    flash = msg;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => { flash = ''; }, 8000);
  }

  // ---- 偏好（持久化到 data/settings.json，首次自动迁移旧 localStorage 值）----
  type Settings = { fontFamily: string; fontSize: number; copyOnSelect: boolean; confirmMultiLine: boolean; asideHidden: boolean; contextMenu: boolean; collapsedFolders: string[]; ctrlEnter: string; palette: string; bright: number; confirmClose: boolean; compactList: boolean; sftpHist: number; confirmSftpDelete: boolean; confirmSftpOverwrite: boolean; editCmd: string; checkUpdate: boolean; skipUpdate: string; sftpWindow: string };
  let prefCopyOnSelect = $state(true);
  let prefConfirmMultiLine = $state(true);
  let prefContextMenu = $state(true);
  let fontSize = $state(14);
  let fontFamily = $state('');
  let fontList = $state<string[] | null>(null);
  let showSettings = $state(false);
  let showAside = $state(true);
  let settingsLoaded = $state(false);
  type VaultStatus = { enabled: boolean; unlocked: boolean };
  let vaultEnabled = $state(false);
  let locked = $state(false);
  let lockPw = $state('');
  let lockErr = $state('');
  let lockBusy = $state(false);
  let collapsedFolders = $state<string[]>([]);
  let prefCtrlEnter = $state('newline');
  let prefPalette = $state('tango');
  let prefBright = $state(0);
  let dataDir = $state('');
  let prefConfirmClose = $state(true);
  let prefCompactList = $state(false);
  let prefSftpHist = $state(8);
  let prefSftpWindow = $state('embedded');
  let prefConfirmSftpDelete = $state(true);
  let prefConfirmSftpOverwrite = $state(false);
  let prefEditCmd = $state('');
  let prefCheckUpdate = $state(true);
  let skipUpdate = $state('');

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  async function saveSettings() {
    if (!settingsLoaded) return;
    try {
      await invoke('set_settings', { settings: {
        fontFamily, fontSize, copyOnSelect: prefCopyOnSelect,
        confirmMultiLine: prefConfirmMultiLine, asideHidden: !showAside,
        contextMenu: prefContextMenu,
        collapsedFolders, ctrlEnter: prefCtrlEnter,
        palette: prefPalette, bright: prefBright, confirmClose: prefConfirmClose, compactList: prefCompactList,
        sftpHist: prefSftpHist, sftpWindow: prefSftpWindow, confirmSftpDelete: prefConfirmSftpDelete,
        confirmSftpOverwrite: prefConfirmSftpOverwrite,
        editCmd: prefEditCmd,
        checkUpdate: prefCheckUpdate, skipUpdate,
      } satisfies Settings });
    } catch { /* 保存失败不阻塞界面 */ }
  }
  function saveSettingsSoon() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(saveSettings, 300);
  }

  // ---- 新版本检测：请求由界面发（走系统 TLS，省一套 HTTPS 依赖），解析与版本比较在后端 ----
  type Release = { version: string; url: string; notes: string };
  // 安装形态由后端探测：便携版没有可替换的安装包，自动更新只能走手动下载
  type InstallKind = 'portable' | 'nsis' | 'appimage' | 'msi' | 'other' | '';
  const PORTABLE_UPDATE_HINT = '便携版请手动下载安装包，安装一次后可自动更新';
  let currentVersion = $state('');
  let update = $state<Release | null>(null);
  let updateDismissed = $state(false);
  let updateChecking = $state(false);
  let updateMsg = $state('');
  let installKind = $state<InstallKind>('');
  let updateOpen = $state(false);
  let autoUpdating = $state(false);
  let autoProgress = $state(0);
  let autoMsg = $state('');

  function normInstallKind(k: string): InstallKind {
    switch (k) {
      case 'portable': case 'nsis': case 'appimage': case 'msi': case 'other': return k;
      default: return '';
    }
  }

  async function checkForUpdate(manual = false) {
    if (updateChecking) return;
    updateChecking = true;
    if (manual) updateMsg = '检查中…';
    try {
      const feed = await invoke<{ api: string; current: string }>('update_feed');
      currentVersion = feed.current;
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), 8000);
      let res: Response;
      try {
        res = await fetch(feed.api, { headers: { Accept: 'application/vnd.github+json' }, signal: ctrl.signal });
      } finally {
        clearTimeout(timer);
      }
      if (!res.ok) {
        // 匿名访问限流是这里最常见的失败，其余状态码原样报出来
        throw new Error(res.status === 403 || res.status === 429 ? 'GitHub 接口限流，稍后再试' : `GitHub 返回 HTTP ${res.status}`);
      }
      const rel = await invoke<Release | null>('update_check', { body: await res.text() });
      if (rel && rel.version !== skipUpdate) {
        update = rel;
        updateDismissed = false;
        autoMsg = '';
        autoProgress = 0;
        // 启动时的静默检查只出提示条；手动检查有结果才直接展开选择窗口
        if (manual) {
          updateMsg = `发现新版本 v${rel.version}（当前 v${feed.current}）`;
          updateOpen = true;
        }
      } else if (manual) {
        updateMsg = rel ? `v${rel.version} 被你忽略过，点「恢复提醒」会再问` : `已是最新版本 v${feed.current}`;
      }
    } catch (e) {
      // 启动时的静默检查失败不打扰，手动检查要把原因显示出来
      if (manual) updateMsg = `检查失败：${e instanceof Error ? e.message : String(e)}`;
    } finally {
      updateChecking = false;
    }
  }

  function openReleasePage() {
    if (!update?.url) return;
    void openUrl(update.url).catch(() => { updateMsg = '打开发布页失败，请到 GitHub 的 Releases 页面手动下载'; });
  }

  function openUpdateDialog() {
    if (autoUpdating) return;
    autoMsg = '';
    autoProgress = 0;
    updateOpen = true;
  }

  // 自动更新只负责下载安装；检测与说明仍以 GitHub API 的结果为准
  async function autoUpdate() {
    if (autoUpdating) return;
    if (installKind === 'portable') { autoMsg = PORTABLE_UPDATE_HINT; return; }
    autoUpdating = true;
    autoProgress = 0;
    autoMsg = '正在准备自动更新…';
    try {
      const au = await checkAutoUpdate();
      if (!au) {
        autoMsg = '没有可用的自动更新包，请手动下载';
        return;
      }
      let total = 0;
      let received = 0;
      await au.downloadAndInstall((ev) => {
        if (ev.event === 'Started') {
          total = ev.data.contentLength ?? 0;
          autoProgress = 0;
          autoMsg = total > 0 ? `正在下载安装包… 0%（共 ${(total / 1048576).toFixed(1)} MB）` : '正在下载安装包…';
        } else if (ev.event === 'Progress') {
          received += ev.data.chunkLength;
          if (total > 0) {
            autoProgress = Math.min(1, received / total);
            autoMsg = `正在下载安装包… ${Math.round(autoProgress * 100)}%`;
          } else {
            autoMsg = `正在下载安装包… ${(received / 1048576).toFixed(1)} MB`;
          }
        } else {
          autoProgress = 1;
          autoMsg = installKind === 'appimage' ? '安装完成，正在重启…' : '安装程序已启动，应用即将退出…';
        }
      });
      if (installKind === 'appimage') {
        await relaunch();
      } else {
        // Windows 下 downloadAndInstall 会直接退出进程，下面这句通常来不及显示
        autoMsg = '安装程序已启动，应用即将退出；若未退出请手动关闭后重新打开';
      }
    } catch (e) {
      autoMsg = `自动更新失败：${e instanceof Error ? e.message : String(e)}。请点「手动下载」到发布页下载安装包。`;
    } finally {
      autoUpdating = false;
    }
  }

  function skipThisVersion() {
    if (!update) return;
    skipUpdate = update.version;
    update = null;
    updateOpen = false;
    saveSettingsSoon();
  }

  function unskipVersion() {
    skipUpdate = '';
    saveSettingsSoon();
    void checkForUpdate(true);
  }

  // 焦点在哪：输入进不了终端多半是焦点落在了列表/SFTP 上，给个显性提示
  let focusZone = $state('');
  function onFocusIn(e: FocusEvent) {
    const t = e.target as HTMLElement | null;
    if (!t) return;
    focusZone = t.closest('.xterm') ? '终端'
      : t.closest('.sftp-side') ? '文件面板'
      : t.closest('aside') ? '服务器列表' : '';
  }

  let unlistenAll: UnlistenFn[] = [];

  const activeTab = $derived(activeIdx >= 0 && activeIdx < tabs.length ? tabs[activeIdx] : null);

  function emptyRdp(): RdpOptions {
    return { sec: 'auto', shareDir: null, clipboard: true, fullscreen: false, ignoreCert: false, width: null, height: null };
  }

  const RES_PRESETS = ['1920x1080', '1600x900', '1440x900', '1366x768', '1280x720', '1024x768'];
  const resValue = $derived.by(() => {
    const { width, height } = draft.rdp;
    if (!width || !height) return '';
    const s = `${width}x${height}`;
    return RES_PRESETS.includes(s) ? s : 'custom';
  });
  function onResChange(v: string) {
    if (v === '') { draft.rdp.width = null; draft.rdp.height = null; }
    else if (v === 'custom') { draft.rdp.width ??= 1280; draft.rdp.height ??= 720; }
    else {
      const [w, h] = v.split('x').map(Number);
      draft.rdp.width = w; draft.rdp.height = h;
    }
  }

  // Rust 端对默认值不落盘（skip_serializing_if），这里补齐
  function norm(p: Profile): Profile {
    return { ...p, icon: p.icon ?? null, rdp: { ...emptyRdp(), ...(p.rdp ?? {}) } };
  }

  function emptyDraft(): Profile {
    return { id: '', name: '', host: '', port: 22, username: 'root',
      auth_method: 'agent', protocol: 'ssh', rdp: emptyRdp(), icon: 'terminal', key_path: null, remark: '', group_tag: null,
      proxy_jump: null, created_at: 0, updated_at: 0 };
  }

  // 密码要走星号遮罩输入框，浏览器 prompt() 是明文的，只能自绘
  let pwPrompt = $state<{ msg: string; value: string; okLabel: string; resolve: (v: string | null) => void } | null>(null);
  function askPassword(msg: string, okLabel = '确定'): Promise<string | null> {
    return new Promise((resolve) => { pwPrompt = { msg, value: '', okLabel, resolve }; });
  }
  function answerPw(ok: boolean) {
    const p = pwPrompt;
    if (!p) return;
    pwPrompt = null;
    p.resolve(ok ? p.value : null);
  }
  function focusEl(el: HTMLInputElement) { el.focus(); }

  async function unlockVault() {
    if (lockBusy) return;
    lockBusy = true;
    lockErr = '';
    try {
      await invoke('vault_unlock', { password: lockPw });
      lockPw = '';
      locked = false;
      await refresh();
    } catch (e) {
      lockErr = String(e);
    } finally {
      lockBusy = false;
    }
  }

  async function enableVault() {
    const p1 = await askPassword('设置启动密码（至少 6 位）。忘记后无法恢复已保存的密码。');
    if (p1 === null) return;
    if (Array.from(p1).length < 6) { error = '启动密码至少 6 位'; return; }
    const p2 = await askPassword('再次输入启动密码');
    if (p2 === null) return;
    if (p1 !== p2) { error = '两次输入的启动密码不一致'; return; }
    try {
      await invoke('vault_enable', { password: p1 });
      vaultEnabled = true;
      alert('启动密码已启用，下次打开 lterm 需要登录。');
    } catch (e) { error = String(e); }
  }

  async function changeVaultPw() {
    const old = await askPassword('输入当前启动密码');
    if (old === null) return;
    const p1 = await askPassword('输入新的启动密码（至少 6 位）');
    if (p1 === null) return;
    if (Array.from(p1).length < 6) { error = '启动密码至少 6 位'; return; }
    const p2 = await askPassword('再次输入新的启动密码');
    if (p2 === null) return;
    if (p1 !== p2) { error = '两次输入的启动密码不一致'; return; }
    try {
      await invoke('vault_change', { oldPassword: old, newPassword: p1 });
      alert('启动密码已修改。');
    } catch (e) { error = String(e); }
  }

  async function disableVault() {
    if (!confirm('关闭启动密码后，已保存的密码会以明文写回 data/secrets.json。确定关闭？')) return;
    const p = await askPassword('输入当前启动密码');
    if (p === null) return;
    try {
      await invoke('vault_disable', { password: p });
      vaultEnabled = false;
      alert('已关闭启动密码，保存的密码现以明文存放。');
    } catch (e) { error = String(e); }
  }

  let savedPwIds = $state<Set<string>>(new Set());

  async function refresh() {
    servers = (await invoke<Profile[]>('list_servers')).map(norm);
    folders = await invoke<string[]>('list_folders');
    manualOrdered = await invoke<boolean>('get_manual_order');
    savedPwIds = new Set(await invoke<string[]>('list_saved_password_ids'));
    search();
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function search() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      hits = query.trim()
        ? (await invoke<Hit[]>('search_servers', { query })).map((h) => ({ ...h, server: norm(h.server) }))
        : servers.map((s) => ({ server: s, score: 0 }));
    }, 80);
  }

  type Section = { tag: string; label: string; items: Hit[] };
  const nameCmp = (a: Profile, b: Profile) => a.name.localeCompare(b.name, 'zh');
  const sameOrder = () => 0;
  // 列表被手动排过序时按数组顺序显示；搜索时仍按名称，避免命中项顺序看不懂
  const keepManualOrder = $derived(manualOrdered && query.trim() === '');
  const groupCmp = $derived<(a: Profile, b: Profile) => number>(keepManualOrder ? sameOrder : nameCmp);
  const groupNames = $derived(
    [...new Set([...folders, ...servers.map((s) => s.group_tag).filter((t): t is string => !!t)])]
      .sort((a, b) => a.localeCompare(b, 'zh'))
  );
  const shareDirHistory = $derived(
    [...new Set(servers.map((s) => s.rdp.shareDir).filter((d): d is string => !!d))]
  );
  // 分组之间固定按名称，未分组永远最后；组内顺序由 cmp 决定
  function groupBy<T>(list: T[], tagOf: (v: T) => string | null, cmp: (a: T, b: T) => number, emptyTags: string[] = []) {
    const m = new Map<string, T[]>();
    for (const t of emptyTags) m.set(t, []);
    for (const v of list) {
      const t = tagOf(v) ?? '';
      const arr = m.get(t);
      if (arr) arr.push(v); else m.set(t, [v]);
    }
    const secs = [...m.keys()].filter((t) => t !== '')
      .sort((a, b) => a.localeCompare(b, 'zh'))
      .map((t) => ({ tag: t, items: m.get(t)!.sort(cmp) }));
    const rest = m.get('');
    if (rest?.length) secs.push({ tag: '', items: rest.sort(cmp) });
    return secs;
  }
  const sections = $derived.by<Section[]>(() =>
    groupBy(hits, (h) => h.server.group_tag, (a, b) => groupCmp(a.server, b.server), query.trim() ? [] : folders)
      .map((g) => ({ tag: g.tag, label: g.tag || '未分组', items: g.items }))
  );
  // 调整顺序以全量列表为基准（搜索时不提供排序入口，不受命中过滤影响）
  const orderedGroups = $derived(groupBy(servers, (s) => s.group_tag, groupCmp));
  const searching = $derived(query.trim() !== '');
  function toggleFolder(tag: string) {
    collapsedFolders = collapsedFolders.includes(tag)
      ? collapsedFolders.filter((t) => t !== tag)
      : [...collapsedFolders, tag];
    saveSettingsSoon();
  }
  async function newFolder() {
    const name = prompt('新建文件夹名称')?.trim();
    if (!name) return;
    try {
      await invoke('add_folder', { name });
      await refresh();
    } catch (e) { error = String(e); }
  }
  async function renameFolder(tag: string) {
    const name = prompt('重命名文件夹', tag)?.trim();
    if (!name || name === tag) return;
    try {
      await invoke('rename_folder', { old: tag, new: name });
      collapsedFolders = collapsedFolders.map((t) => (t === tag ? name : t));
      await refresh();
    } catch (e) { error = String(e); }
  }
  async function deleteFolder(tag: string) {
    const n = sections.find((s) => s.tag === tag)?.items.length ?? 0;
    if (!confirm(n ? `删除文件夹「${tag}」？其中 ${n} 台服务器将退回「未分组」（服务器本身不删）`
                   : `删除空文件夹「${tag}」？`)) return;
    try {
      await invoke('delete_folder', { name: tag });
      collapsedFolders = collapsedFolders.filter((t) => t !== tag);
      await refresh();
    } catch (e) { error = String(e); }
  }

  async function applyOrder(orderedIds: string[], manual: boolean) {
    try {
      await invoke('set_server_order', { orderedIds, manual });
      manualOrdered = manual;
      // 本地就地重排，不等后端回读：连点两次上移不该被上一帧的旧快照吞掉
      const pos = new Map(orderedIds.map((id, i) => [id, i]));
      servers = [...servers].sort((a, b) => (pos.get(a.id) ?? 1e9) - (pos.get(b.id) ?? 1e9));
      search();
    } catch (e) { error = String(e); }
  }

  // 组内把第 from 条挪到第 to 条（to 按挪走之前的下标算）
  function reorderWithin(tag: string, from: number, rawTo: number) {
    const groups = orderedGroups.map((g) => ({ tag: g.tag, items: [...g.items] }));
    const g = groups.find((x) => x.tag === tag);
    if (!g) return;
    const to = Math.max(0, Math.min(rawTo > from ? rawTo - 1 : rawTo, g.items.length - 1));
    if (to === from) return;
    const [moved] = g.items.splice(from, 1);
    g.items.splice(to, 0, moved);
    void applyOrder(groups.flatMap((x) => x.items.map((s) => s.id)), true);
  }

  function moveServer(id: string, delta: -1 | 1) {
    const g = orderedGroups.find((x) => x.items.some((s) => s.id === id));
    if (!g) return;
    const i = g.items.findIndex((s) => s.id === id);
    reorderWithin(g.tag, i, i + delta);
  }

  function resetOrder() {
    if (!servers.length) return;
    if (!confirm('按名称重新排列服务器列表？手动调过的顺序会丢失。')) return;
    void applyOrder([...servers].sort(nameCmp).map((s) => s.id), false);
  }

  // ---- 拖动排序 ----
  // 不用 HTML5 draggable：Tauri 默认 dragDropEnabled=true 会截走 webview 的拖放事件（SFTP 传文件靠它），所以走 pointer 事件自绘
  type Drag = { id: string; tag: string; from: number; y0: number; live: boolean };
  let drag = $state<Drag | null>(null);
  // 落点：组内插入用的下标（按挪走之前的行号算）
  let dropAt = $state<number | null>(null);
  let listEl: HTMLUListElement | undefined = $state();

  function gripDown(e: PointerEvent, hit: Hit, sec: Section, i: number) {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = { id: hit.server.id, tag: sec.tag, from: i, y0: e.clientY, live: false };
    dropAt = null;
  }
  function gripMove(e: PointerEvent) {
    const d = drag;
    if (!d) return;
    // 起手先留点容差，免得手抖一下就把双击连接变成挪位置
    if (!d.live) {
      if (Math.abs(e.clientY - d.y0) < 4) return;
      d.live = true;
    }
    // 拖到列表上下边缘时自动滚动，条目多的时候不用为了挪一台来回拖好几趟
    const box = listEl?.getBoundingClientRect();
    let inEdge = false;
    if (box && listEl) {
      inEdge = e.clientY < box.top + 28 || e.clientY > box.bottom - 28;
      if (e.clientY < box.top + 28) listEl.scrollTop -= 14;
      else if (e.clientY > box.bottom - 28) listEl.scrollTop += 14;
    }
    const li = (document.elementsFromPoint(e.clientX, e.clientY) as HTMLElement[])
      .find((el) => el.classList.contains('member') && el.dataset.tag === d.tag);
    if (li) {
      const r = li.getBoundingClientRect();
      const idx = Number(li.dataset.idx);
      dropAt = e.clientY < r.top + r.height / 2 ? idx : idx + 1;
    } else if (!inEdge) {
      // 滚到边缘时指针常会压在组头上，这种时候留住上一个落点；真正拖到别的组就清掉，表示不能放这儿
      dropAt = null;
    }
  }
  function gripUp(e: PointerEvent) {
    const d = drag, at = dropAt;
    drag = null; dropAt = null;
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture?.(e.pointerId)) el.releasePointerCapture(e.pointerId);
    if (!d?.live || at === null) return;
    reorderWithin(d.tag, d.from, at);
  }

  async function save() {
    try {
      const saved = await invoke<Profile>('upsert_server', { profile: draft });
      const wantsSecret = draft.protocol === 'rdp' || draft.auth_method === 'password' || draft.auth_method === 'key';
      if (draftClearPw) await invoke('set_server_secret', { id: saved.id, secret: null });
      else if (wantsSecret && draftSecret && draftSavePw) await invoke('set_server_secret', { id: saved.id, secret: draftSecret });
      draftSecret = '';
      draftSavePw = false;
      draftClearPw = false;
      closeForm();
      await refresh();
    } catch (e) { error = String(e); }
  }

  // 表单关掉后焦点不该留在遮罩里，交还给终端（没有会话时保持原样）
  function closeForm() {
    showForm = false;
    draft = emptyDraft();
    draftSecret = '';
    draftSavePw = false;
    draftClearPw = false;
    focusActive();
  }

  function edit(p: Profile) {
    draft = { ...p };
    draftSecret = '';
    draftSavePw = false;
    draftClearPw = false;
    showForm = true;
  }

  function onProtocolChange(v: string) {
    draft.protocol = v === 'rdp' ? 'rdp' : 'ssh';
    // 图标还是协议默认（或没设）时跟着协议走，方便一眼区分 SSH / RDP；自定义过则保留
    if (!draft.icon || draft.icon === 'terminal' || draft.icon === 'desktop')
      draft.icon = draft.protocol === 'rdp' ? 'desktop' : 'terminal';
    if (draft.protocol === 'rdp') {
      if (draft.port === 22) draft.port = 3389;
      draft.auth_method = 'password'; // 密钥/agent 对 RDP 无意义
    } else if (draft.port === 3389) {
      draft.port = 22;
    }
  }

  async function remove(id: string) {
    if (!confirm('删除该服务器？')) return;
    try {
      await invoke('delete_server', { id });
      await refresh();
    } catch (e) { error = String(e); }
  }

  function copyName(base: string) {
    // 复制一个副本时回到原名重新编号，避免名字越叠越长
    const root = base.replace(/-复制\d*$/, '');
    const taken = new Set(servers.map((s) => s.name));
    let name = `${root}-复制`;
    for (let i = 2; taken.has(name); i++) name = `${root}-复制${i}`;
    return name;
  }

  // 复制只是把配置载入编辑表单（id 置空、名称加“复制”后缀），点保存才真正新增，取消则不留痕
  function duplicate(p: Profile) {
    draft = { ...p, id: '', name: copyName(p.name), created_at: 0, updated_at: 0 };
    draftSecret = '';
    draftSavePw = false;
    draftClearPw = false;
    showForm = true;
  }

  function setIcon(v: string) {
    // 认不出的名字也保留（比如以后扩充图标集），只是暂时不渲染
    draft.icon = normalizeIcon(v) || null;
    showIconPicker = false;
  }

  // 列表双击、表单关闭、面板收起后都把焦点交还终端；已在终端则不动
  const noFocus = (e: MouseEvent) => e.preventDefault();
  function focusActive() {
    const t = tabs[activeIdx];
    if (!t) return;
    const el = document.activeElement;
    if (el && el.closest('.xterm')) return;
    const host = paneEl?.querySelector('.xterm');
    const input = host?.querySelector<HTMLElement>('.xterm-helper-textarea')
      ?? host?.querySelector<HTMLElement>('textarea');
    input?.focus();
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
    if (connecting.has(p.id)) return;
    error = '';
    markConnecting(p);
    try {
      await connectInner(p);
    } finally {
      stopConnecting(p.id);
    }
  }

  async function connectInner(p: Profile) {
    if (p.protocol === 'rdp') { await connectRdp(p); return; }
    let chain: ChainHop[];
    try {
      chain = await invoke<ChainHop[]>('get_proxy_chain', { profileId: p.id });
    } catch (e) { error = String(e); return; }
    const pwMap: Record<string, string> = {};
    for (const h of chain) {
      if (h.has_saved_secret) continue;
      const label = hopPromptLabel(h);
      if (!label) continue;
      const pw = await askPassword(label);
      if (pw === null) return; // 取消
      pwMap[h.id] = pw;
    }
    const { cols, rows } = estimateSize();
    try {
      const sid = await invoke<string>('connect', {
        profileId: p.id, password: pwMap[p.id] ?? null, proxyPasswords: pwMap, cols, rows,
      });
      tabs = [...tabs, { sid, profile: p, alive: true, password: pwMap[p.id] ?? null, passwords: pwMap }];
      activeIdx = tabs.length - 1;
      flushNotices();
      focusActive();
    } catch (e) {
      error = String(e);
    }
  }

  const isWindowsUA = /Windows/i.test(navigator.userAgent);

  async function connectRdp(p: Profile) {
    // Windows 上 mstsc 自己弹凭据框（微软没有提供命令行传密码的途径），这里不多问一次；
    // Linux 上把密码经管道交给 xfreerdp 的 /from-stdin，不进 argv，也不写盘。
    let pwMap: Record<string, string> = {};
    if (p.proxy_jump) {
      let chain: ChainHop[];
      try { chain = await invoke<ChainHop[]>('get_proxy_chain', { profileId: p.id }); }
      catch (e) { error = String(e); return; }
      for (const h of chain) {
        if (h.id === p.id) continue;
        if (h.has_saved_secret) continue;
        const label = hopPromptLabel(h, '跳板密码');
        if (!label) continue;
        const pw = await askPassword(label);
        if (pw === null) return;
        pwMap[h.id] = pw;
      }
    }
    let pw: string | null = null;
    if (!isWindowsUA && !savedPwIds.has(p.id)) {
      const typed = await askPassword(`密码 (${p.name} · ${p.username}@${p.host}:${p.port})`);
      if (typed === null) return;
      pw = typed;
    }
    try {
      showFlash(await invoke<string>('open_rdp', { profileId: p.id, password: pw, proxyPasswords: pwMap }));
    } catch (e) {
      error = String(e);
    }
  }

  async function closeTab(i: number) {
    const t = tabs[i];
    if (!t) return;
    if (t.alive && prefConfirmClose && !confirm(`断开 ${t.profile.username}@${t.profile.host}？传输中的任务会一起中断。`)) return;
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
    saveSettingsSoon();
    const t = activeTab;
    if (t) requestAnimationFrame(() => fitFns.get(t.sid)?.());
  }

  function onZoom(e: Event) {
    const d = (e as CustomEvent<number>).detail;
    setFont(d === 999 ? 999 : fontSize + d);
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (locked) return;
    if (e.key === 'Escape' && showSettings && !showForm && !pwPrompt) {
      closeSettings();
      return;
    }
    if (e.key === 'Escape' && showForm && !pwPrompt) {
      closeForm();
      return;
    }
    if (e.key === 'F12') {
      e.preventDefault();
      invoke('toggle_devtools').catch(() => {});
      return;
    }
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
      saveSettingsSoon();
    } else if (e.ctrlKey && e.key.toLowerCase() === 'w') {
      if (activeIdx >= 0) { e.preventDefault(); closeTab(activeIdx); }
    }
  }

  onMount(async () => {
    // 放最前面：启动早期就点 X 也该被问住，而不是等下面几个 await 跑完才挂上
    unlistenAll.push(await getCurrentWindow().onCloseRequested(async (ev) => {
      // 只要有 JS 监听这个事件，Tauri 就不会自己关窗口，所以先拦下，退不退交给 resolve_exit 说
      ev.preventDefault();
      const names = tabs.filter((t) => t.alive).map((t) => t.profile.name);
      if (!names.length) {
        await invoke('resolve_exit', { allow: true });
        return;
      }
      const who = `（${names.slice(0, 3).join('、')}${names.length > 3 ? ' 等' : ''}）`;
      const ok = confirm(`还有 ${names.length} 个连接在用${who}，退出会全部断开，传输中的任务也会一起中断。确定退出？`);
      await invoke('resolve_exit', { allow: ok });
    }));
    window.addEventListener('resize', onWinResize);
    window.addEventListener('lterm-zoom', onZoom);
    window.addEventListener('keydown', onWindowKeydown);
    window.addEventListener('focusin', onFocusIn);
    try {
      const v = await invoke<VaultStatus>('vault_status');
      vaultEnabled = v.enabled;
      locked = v.enabled && !v.unlocked;
    } catch { /* 旧版本没有启动密码 */ }
    try {
      let s = await invoke<Settings>('get_settings');
      if (!localStorage.getItem('lterm-settings-migrated')) {
        const oldFont = localStorage.getItem('fontFamily');
        const oldSize = Number(localStorage.getItem('fontSize') ?? 0);
        const oldAside = localStorage.getItem('asideHidden');
        if (oldFont || oldSize || oldAside !== null) {
          s = {
            ...s,
            fontFamily: oldFont ?? s.fontFamily,
            fontSize: oldSize >= 10 && oldSize <= 28 ? oldSize : s.fontSize,
            asideHidden: oldAside === '1',
            copyOnSelect: (localStorage.getItem('prefCopyOnSelect') ?? '1') === '1',
            confirmMultiLine: (localStorage.getItem('prefConfirmMultiLine') ?? '1') === '1',
          };
          await invoke('set_settings', { settings: s }).catch(() => {});
        }
        localStorage.setItem('lterm-settings-migrated', '1');
      }
      fontFamily = s.fontFamily;
      fontSize = s.fontSize;
      showAside = !s.asideHidden;
      prefCopyOnSelect = s.copyOnSelect;
      prefConfirmMultiLine = s.confirmMultiLine;
      prefContextMenu = s.contextMenu;
      collapsedFolders = s.collapsedFolders ?? [];
      prefCtrlEnter = s.ctrlEnter || 'newline';
      prefPalette = s.palette || 'tango';
      prefBright = s.bright || 0;
      prefConfirmClose = s.confirmClose !== false;
      prefCompactList = !!s.compactList;
      prefSftpHist = Math.min(30, Math.max(3, s.sftpHist || 8));
      prefSftpWindow = s.sftpWindow === 'window' ? 'window' : 'embedded';
      prefConfirmSftpDelete = s.confirmSftpDelete !== false;
      prefConfirmSftpOverwrite = s.confirmSftpOverwrite === true;
      prefEditCmd = s.editCmd ?? '';
      prefCheckUpdate = s.checkUpdate !== false;
      skipUpdate = s.skipUpdate ?? '';
    } catch { /* 用默认值 */ }
    settingsLoaded = true;
    void invoke<{ current: string }>('update_feed').then((f) => { currentVersion = f.current; }).catch(() => {});
    // 安装形态取不到时按未知处理：页面照常用，只是自动更新会走运行时错误兜底
    try {
      installKind = normInstallKind(await invoke<string>('install_kind'));
    } catch { installKind = ''; }
    // 晚一点再问：别和恢复窗口、首屏渲染抢那点带宽，也别在用户还在挑服务器时就弹框
    if (prefCheckUpdate) setTimeout(() => void checkForUpdate(), 1500);
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

  onDestroy(() => {
    clearTimeout(flashTimer);
    window.removeEventListener('focusin', onFocusIn);
    window.removeEventListener('resize', onWinResize);
    window.removeEventListener('lterm-zoom', onZoom);
    window.removeEventListener('keydown', onWindowKeydown);
    for (const u of unlistenAll) u();
  });

  function setSftpHist(e: Event) {
    const v = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(v)) prefSftpHist = Math.min(30, Math.max(3, Math.round(v)));
    saveSettingsSoon();
  }

  function closeSettings() {
    showSettings = false;
    focusActive();
  }

  async function loadSettingsPath() {
    if (dataDir) return;
    try { dataDir = await invoke<string>('get_data_dir'); } catch { /* 拿不到就不显示 */ }
  }
  async function openSettings() {
    showSettings = !showSettings;
    if (showSettings) void loadSettingsPath();
    if (showSettings && !fontList) {
      try { fontList = await invoke<string[]>('list_fonts'); } catch { fontList = []; }
    }
    if (!showSettings) focusActive();
  }

  function setFontFamily(v: string) {
    fontFamily = v;
    saveSettingsSoon();
  }

  function toggleAside(v: boolean) {
    showAside = v;
    saveSettingsSoon();
  }

  const authLabel = { password: '密码', key: '密钥', agent: 'agent' } as const;

  // 服务器条目的上下文菜单（两种列表风格都用它）
  let srvCtx = $state<{ x: number; y: number; srv: Profile } | null>(null);
  const CTX_W = 132, CTX_H = 196;
  function openSrvCtx(e: MouseEvent, srv: Profile) {
    e.preventDefault();
    srvCtx = {
      x: Math.max(4, Math.min(e.clientX, window.innerWidth - CTX_W)),
      // 贴底时往上挪，别让菜单被裁掉
      y: Math.max(4, Math.min(e.clientY, window.innerHeight - CTX_H)),
      srv,
    };
  }
  function srvAct(fn: (p: Profile) => void | Promise<void>) {
    const p = srvCtx?.srv;
    srvCtx = null;
    if (p) void fn(p);
  }
  $effect(() => {
    if (!srvCtx) return;
    const away = (e: PointerEvent) => {
      if (!(e.target as HTMLElement | null)?.closest?.('.srvctx')) srvCtx = null;
    };
    const esc = (e: KeyboardEvent) => { if (e.key === 'Escape') srvCtx = null; };
    // 捕获阶段：菜单开着时点别处或按 Esc 都要能关，xterm 会吞掉冒泡阶段的 Esc
    window.addEventListener('pointerdown', away, true);
    window.addEventListener('keydown', esc, true);
    return () => {
      window.removeEventListener('pointerdown', away, true);
      window.removeEventListener('keydown', esc, true);
    };
  });
</script>

<main>
  {#if !showAside}
    <button class="rail" onmousedown={noFocus} onclick={() => toggleAside(true)} title="显示服务器列表 (Ctrl+B)" aria-label="显示服务器列表">»</button>
  {/if}
  <aside style:display={showAside ? 'flex' : 'none'}>
    <div class="toolbar">
      <input class="search" placeholder="模糊搜索：名称 备注 用户 主机…" bind:value={query} oninput={search} />
      <button onmousedown={noFocus} onclick={() => toggleAside(false)} title="隐藏列表 (Ctrl+B)">⟨</button>
    </div>
    <ul class="server-list" class:dragging={!!drag?.live} bind:this={listEl}>
      {#each sections as sec (sec.tag)}
        {#if sec.tag !== '' || sections.length > 1}
          <li class="folder-head">
            <button class="folder" onmousedown={noFocus} onclick={() => toggleFolder(sec.tag)}
                    title={collapsedFolders.includes(sec.tag) && !searching ? '展开' : '收起'}>
              <span class="fold">{collapsedFolders.includes(sec.tag) && !searching ? '▸' : '▾'}</span>
              <span class="fname">{sec.label}</span>
              <span class="fcount">{sec.items.length}</span>
            </button>
            <span class="folds">
              {#if sec.tag !== ''}
                <button class="icon foldact" onclick={() => renameFolder(sec.tag)} title="重命名文件夹">✎</button>
                <button class="del foldact" onclick={() => deleteFolder(sec.tag)} title="删除文件夹">×</button>
              {/if}
            </span>
          </li>
        {/if}
        {#if searching || !collapsedFolders.includes(sec.tag)}
          {#each sec.items as hit, i (hit.server.id)}
            <li class="member" class:compact={prefCompactList}
                class:dragging={drag?.id === hit.server.id}
                class:drop-top={drag?.live && drag.tag === sec.tag && dropAt === i}
                class:drop-bottom={drag?.live && drag.tag === sec.tag && dropAt === i + 1 && i === sec.items.length - 1}
                data-tag={sec.tag} data-idx={i}
                oncontextmenu={(e) => openSrvCtx(e, hit.server)}>
              <button class="server" class:busy={connectingIds.has(hit.server.id)}
                      onmousedown={noFocus}
                      ondblclick={() => connect(hit.server)}
                      onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); connect(hit.server); } }}
                      title={connectingIds.has(hit.server.id) ? '正在建立连接…' : '双击连接 · 右键菜单'}>
                <span class="titlerow">
                  {#if !searching}
                    <span class="grip" title="按住拖动可在组内排序" role="presentation"
                          onpointerdown={(e) => gripDown(e, hit, sec, i)}
                          onpointermove={gripMove}
                          onpointerup={gripUp} onpointercancel={gripUp}>⠿</span>
                  {/if}
                  {#if hit.server.icon && iconPaths(hit.server.icon)}
                    <span class="sicon"><Icon name={hit.server.icon} size={15} /></span>
                  {/if}
                  <span class="name">{hit.server.name}</span>
                  {#if connectingIds.has(hit.server.id)}<span class="spin"></span><span class="ing">连接中</span>{/if}
                </span>
                {#if !prefCompactList}
                  <span class="meta">{hit.server.username}@{hit.server.host} · {hit.server.protocol === 'rdp' ? 'RDP' : authLabel[hit.server.auth_method]}</span>
                  {#if hit.server.remark}<span class="remark">{hit.server.remark}</span>{/if}
                {/if}
              </button>
              {#if !prefCompactList}<span class="acts">
                {#if !searching}
                  <button class="icon" disabled={i === 0} onclick={() => moveServer(hit.server.id, -1)} title="上移（组内）">↑</button>
                  <button class="icon" disabled={i === sec.items.length - 1} onclick={() => moveServer(hit.server.id, 1)} title="下移（组内）">↓</button>
                {/if}
                <button class="icon" onclick={() => edit(hit.server)} title="编辑">✎</button>
                <button class="icon" onclick={() => duplicate(hit.server)} title="复制到编辑窗口，保存后才新增"><Icon name="copy" size={13} /></button>
                <button class="del" onclick={() => remove(hit.server.id)} title="删除">×</button>
              </span>{/if}
            </li>
          {/each}
        {/if}
      {:else}
        <li class="empty">暂无服务器，点下面「＋ 添加」</li>
      {/each}
    </ul>
    <div class="listbar">
      <button onmousedown={noFocus} onclick={() => { if (showForm) { showForm = false; focusActive(); } else { draft = emptyDraft(); showForm = true; } }} title="添加服务器">＋ 添加</button>
      <button onmousedown={noFocus} onclick={newFolder} title="新建文件夹（分组）">⊞ 分组</button>
      <button onmousedown={noFocus} disabled={!manualOrdered} onclick={resetOrder}
              title={manualOrdered ? '列表被手动排过序，点此按名称重排' : '列表还没手动排过序'}>↺ 重排</button>
    </div>
  </aside>

  {#if srvCtx}
    <div class="srvctx" role="menu" aria-label="服务器操作" style:left={srvCtx.x + 'px'} style:top={srvCtx.y + 'px'}>
      <button role="menuitem" class="ctxi" onmousedown={noFocus} onclick={() => srvAct(connect)}>连接</button>
      <div class="ctxsep" role="separator"></div>
      <button role="menuitem" class="ctxi" onmousedown={noFocus} onclick={() => srvAct(edit)}>编辑</button>
      <button role="menuitem" class="ctxi" onmousedown={noFocus} onclick={() => srvAct(duplicate)}>复制</button>
      <div class="ctxsep" role="separator"></div>
      <button role="menuitem" class="ctxi" onmousedown={noFocus} onclick={() => srvAct((p) => moveServer(p.id, -1))}>上移</button>
      <button role="menuitem" class="ctxi" onmousedown={noFocus} onclick={() => srvAct((p) => moveServer(p.id, 1))}>下移</button>
      <div class="ctxsep" role="separator"></div>
      <button role="menuitem" class="ctxi danger" onmousedown={noFocus} onclick={() => srvAct((p) => remove(p.id))}>删除</button>
    </div>
  {/if}

  <section class="right">
    <div class="tabbar">
      {#each tabs as t, i}
        <div class="tab" class:active={i === activeIdx} role="tab" tabindex={0} aria-selected={i === activeIdx}
             onclick={() => { activeIdx = i; requestAnimationFrame(() => fitFns.get(t.sid)?.()); }}
             onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activeIdx = i; requestAnimationFrame(() => fitFns.get(t.sid)?.()); } }}>
          <span class="dot" class:connected={t.alive} class:closed={!t.alive}></span>
          {t.profile.name}
          <button class="close" aria-label={`关闭标签 ${t.profile.name}`}
                  onmousedown={noFocus}
                  onclick={(e) => { e.stopPropagation(); closeTab(i); }}>×</button>
        </div>
      {/each}
      {#if tabs.length}
        <button class="tab newtab" onclick={() => { query = ''; document.querySelector<HTMLInputElement>('.search')?.focus(); }} title="从列表选择服务器新建会话">＋</button>
      {/if}
    </div>

    <div class="panes" bind:this={paneEl}>
      {#if tabs.length === 0}
        <div class="welcome">
          <h2>lterm</h2>
          <p>左侧添加服务器，双击连接。支持 ssh-agent / 密钥 / 密码认证，RDP 主机交给系统远程桌面客户端。</p>
          <p class="kbd">Ctrl+Tab 切换标签 · Ctrl+F 搜索 · Ctrl+Shift+C/V 复制/粘贴 · Ctrl+W 关闭标签 · 右键菜单（可在设置中改为直接复制/粘贴）</p>
        </div>
      {/if}
      {#each tabs as t, i (t.sid)}
        <Session sessionId={t.sid}
                 profileId={t.profile.id}
                 sessionPassword={t.password}
                 sessionPasswords={t.passwords}
                 prefSftpWindow={prefSftpWindow}
                 profileName={t.profile.name}
                 {fontFamily}
                 active={i === activeIdx}
                 {fontSize}
                 {prefPalette}
                 {prefBright}
                 {prefSftpHist}
                 {prefConfirmSftpDelete}
                 {prefConfirmSftpOverwrite}
                 {prefCopyOnSelect}
                 {prefConfirmMultiLine}
                 prefContextMenu={prefContextMenu}
                 prefCtrlEnter={prefCtrlEnter}
                 onClone={() => activeTab && connect(activeTab.profile)}
                 onClosed={() => onTabClosed(t.sid)}
                 onFocusBack={focusActive}
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
      {#if flash}<span class="flash">{flash}</span>{/if}
      {#if tabs.length && focusZone !== '终端' && focusZone !== ''}
        <span class="focuswarn" title="单击终端区域即可把焦点切回终端">焦点在{focusZone}，输入不进终端</span>
      {/if}
      <span class="spacer"></span>
      {#if connectingMsg}
        <span class="connecting"><span class="spin"></span>{connectingMsg}</span>
      {/if}
      <button class="gear" onmousedown={noFocus} onclick={() => toggleAside(!showAside)} title="显示/隐藏服务器列表 (Ctrl+B)">☰ 列表</button>
      <button class="gear" onmousedown={noFocus} onclick={openSettings} title="终端设置">⚙ 设置</button>
    </div>

    <!-- 错误用浮层盖在终端上，不占布局，避免终端被挤变形/出滚动条 -->
    {#if error}
      <div class="errtoast" role="alert">
        <span>{error}</span>
        <button class="dismiss" onclick={() => error = ''}>✕</button>
      </div>
    {/if}
    {#if update && !updateDismissed}
      <div class="updatetoast" role="status">
        <span class="uttitle">发现新版本 v{update.version}</span>
        <span class="utsub">当前 v{currentVersion || '…'}，可手动下载或自动更新</span>
        <button class="utact" onclick={openUpdateDialog}>查看更新</button>
        <button class="utact" onclick={skipThisVersion} title="这个版本不再提醒，出了更新的版本会再问">忽略此版本</button>
        <button class="dismiss" onclick={() => updateDismissed = true} title="这次先不，下次启动再提醒">✕</button>
      </div>
    {/if}
    {#if updateOpen && update}
      <div class="setmask updmask" role="presentation" onclick={() => updateOpen = false}>
        <div class="setdlg upddlg" role="dialog" aria-modal="true" aria-label="发现新版本" tabindex={-1}
             onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (updateOpen = false)}>
          <div class="shead">
            <span>发现新版本 v{update.version}</span>
            <button class="sclose" onmousedown={noFocus} onclick={() => updateOpen = false} aria-label="关闭更新窗口">✕</button>
          </div>
          <div class="updmeta">当前 v{currentVersion || '…'} · 最新 v{update.version}</div>
          {#if installKind === 'portable'}
            <div class="updhint">{PORTABLE_UPDATE_HINT}</div>
          {/if}
          <pre class="updnotes">{update.notes || '（此版本没有更新说明）'}</pre>
          {#if autoMsg}<div class="supd">{autoMsg}</div>{/if}
          {#if autoUpdating || autoProgress > 0}
            <div class="updbar" role="progressbar" aria-valuemin="0" aria-valuemax="100"
                 aria-valuenow={Math.round(autoProgress * 100)} aria-label="自动更新进度">
              <span style={`width: ${Math.round(autoProgress * 100)}%`}></span>
            </div>
          {/if}
          <div class="updacts">
            <button disabled={autoUpdating} onclick={openReleasePage}>手动下载</button>
            <button disabled={autoUpdating || installKind === 'portable'}
                    title={installKind === 'portable' ? PORTABLE_UPDATE_HINT : '下载安装包并自动安装'}
                    onclick={autoUpdate}>{autoUpdating ? '更新中…' : '自动更新'}</button>
            <span class="spacer"></span>
            <button disabled={autoUpdating} onclick={skipThisVersion}>忽略此版本</button>
            <button disabled={autoUpdating} onclick={() => updateOpen = false}>稍后</button>
          </div>
        </div>
      </div>
    {/if}
  </section>

  {#if showSettings}
    <div class="setmask" role="presentation" onclick={closeSettings}>
      <div class="setdlg" role="dialog" aria-modal="true" aria-label="终端设置" tabindex={-1}
           onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && closeSettings()}>
        <div class="shead">
          <span>终端设置</span>
          <button class="sclose" onmousedown={noFocus} onclick={closeSettings} aria-label="关闭设置">✕</button>
        </div>

        <div class="sec">外观</div>
        <div class="srow">
          <span class="lbl">字号</span>
          <button onclick={() => setFont(fontSize - 1)}>−</button>
          <span class="num">{fontSize}</span>
          <button onclick={() => setFont(fontSize + 1)}>＋</button>
        </div>
        <div class="srow">
          <span class="lbl">字体</span>
          <input class="fontin" list="fontlist" placeholder="默认（留空）" value={fontFamily}
                 aria-label="终端字体"
                 onchange={(e) => setFontFamily((e.target as HTMLInputElement).value)} />
          <datalist id="fontlist">
            {#each (fontList ?? []) as f (f)}<option value={f}></option>{/each}
          </datalist>
        </div>
        <div class="srow">
          <span class="lbl">配色</span>
          <select class="ctlsel" bind:value={prefPalette} onchange={saveSettingsSoon}>
            {#each Object.entries(PALETTE_LABELS) as [key, label] (key)}
              <option value={key}>{label}</option>
            {/each}
          </select>
        </div>
        <div class="srow">
          <span class="lbl">增亮</span>
          <select class="ctlsel" bind:value={prefBright} onchange={saveSettingsSoon}>
            <option value={0}>关</option>
            <option value={1}>弱（γ1.15）</option>
            <option value={2}>中（γ1.3）</option>
            <option value={3}>强（γ1.5）</option>
          </select>
        </div>
        <div class="hint">配色换的是那 16 个 ANSI 色（含背景）；程序自己写死的真彩不走这张表。字体需系统已安装（如 MesloLGS NF），可直接输入名称。改动即时生效并保存。</div>

        <div class="sec">行为</div>
        <label class="ck"><input type="checkbox" bind:checked={prefCopyOnSelect} onchange={saveSettingsSoon} /> 选中即复制</label>
        <label class="ck"><input type="checkbox" bind:checked={prefConfirmMultiLine} onchange={saveSettingsSoon} /> 多行粘贴确认</label>
        <label class="ck"><input type="checkbox" bind:checked={prefContextMenu} onchange={saveSettingsSoon} /> 右键菜单（复制/粘贴/克隆；关闭则右键=复制/粘贴）</label>
        <label class="ck"><input type="checkbox" bind:checked={prefConfirmClose} onchange={saveSettingsSoon} /> 关闭在线会话时确认（只影响标签页；窗口上还有连接时，退出总归会问一次）</label>
        <label class="ck"><input type="checkbox" bind:checked={prefCompactList} onchange={saveSettingsSoon} /> 服务器列表紧凑模式（每行只显示标题，编辑/复制/删除走右键菜单）</label>
        <label class="ck"><input type="checkbox" bind:checked={prefConfirmSftpDelete} onchange={saveSettingsSoon} /> SFTP 面板删除文件前确认（多选只问一次；关掉连目录也不问）</label>
        <label class="ck"><input type="checkbox" bind:checked={prefConfirmSftpOverwrite} onchange={saveSettingsSoon} /> SFTP 传输覆盖同名文件前确认（关掉就是直接覆盖）</label>
        <div class="srow">
          <span class="lbl">SFTP 历史</span>
          <input class="numin" type="number" min="3" max="30" step="1" value={prefSftpHist} aria-label="路径框历史目录条数"
                 onchange={setSftpHist} />
          <span class="unit">条（3~30）· 路径框 ▾ 里记多少个传过的目录</span>
        </div>
        <div class="srow">
          <span class="lbl">SFTP 打开方式</span>
          <select class="ctlsel" bind:value={prefSftpWindow} onchange={saveSettingsSoon}>
            <option value="embedded">内嵌面板</option>
            <option value="window">独立窗口</option>
          </select>
        </div>
        <div class="srow">
          <span class="lbl">SFTP 编辑器</span>
          <input class="fontin" placeholder="默认（系统关联程序）" value={prefEditCmd}
                 aria-label="SFTP 外部编辑器命令"
                 onchange={(e) => { prefEditCmd = (e.target as HTMLInputElement).value; saveSettingsSoon(); }} />
        </div>
        <div class="hint">右键 SFTP 文件「编辑（文本）」会用此程序打开临时文件，保存后约 1-2 秒自动同步回远端。支持 <code>{'{file}'}</code> 占位符（留空则用系统默认程序），例如 <code>code --wait {'{file}'}</code>、<code>gedit {'{file}'}</code>。同步由应用轮询完成，不依赖编辑器是否阻塞。</div>

        <div class="sec">安全</div>
        <div class="srow">
          <span class="lbl">启动密码</span>
          {#if vaultEnabled}
            <span class="unit">已启用 · 启动时需登录</span>
            <button onclick={changeVaultPw}>修改</button>
            <button onclick={disableVault}>关闭</button>
          {:else}
            <span class="unit">未启用 · 保存的密码为明文</span>
            <button onclick={enableVault}>启用</button>
          {/if}
        </div>
        <div class="hint">启用后，服务器密码用启动密码经 Argon2id 派生出的密钥加密存放（AES-256-GCM），启动密码本身不落盘；每次打开 lterm 都要输入一次。忘记启动密码将无法恢复已保存的密码。</div>

        <div class="sec">键盘</div>
        <div class="srow">
          <span class="lbl">Ctrl+Enter</span>
          <select class="ctlsel" bind:value={prefCtrlEnter} onchange={saveSettingsSoon}>
            <option value="newline">换行（发 Alt+Enter 的字节）</option>
            <option value="csiu">换行（Kitty CSI-u 编码）</option>
            <option value="same">不拦截（等同 Enter）</option>
          </select>
        </div>
        <div class="hint">终端协议里 Ctrl+Enter 和 Enter 是同一字节，opencode 这类 TUI 需要改写才能收到换行。</div>

        <div class="sec">更新</div>
        <div class="srow">
          <span class="lbl">当前版本</span>
          <span class="num ver">v{currentVersion || '…'}</span>
          <button disabled={updateChecking} onclick={() => checkForUpdate(true)}>{updateChecking ? '检查中…' : '立即检查'}</button>
          {#if skipUpdate}
            <button onclick={unskipVersion}>恢复提醒（已忽略 v{skipUpdate}）</button>
          {/if}
        </div>
        {#if updateMsg}<div class="supd">{updateMsg}</div>{/if}
        <label class="ck"><input type="checkbox" bind:checked={prefCheckUpdate} onchange={saveSettingsSoon} /> 启动时自动检查新版本（只访问 GitHub Releases 接口，不发任何本机信息）</label>

        <div class="spath" title="设置、服务器列表、known_hosts 都存在这里">设置保存在：<code>{dataDir || '…'}</code></div>
      </div>
    </div>
  {/if}

  {#if showForm}
    <div class="formmask">
      <form class="server-form" onsubmit={(e) => { e.preventDefault(); save(); }}>
        <div class="fhead">
          <span>{draft.id ? '编辑服务器' : '添加服务器'}</span>
          <button type="button" class="del" onclick={closeForm} title="关闭">✕</button>
        </div>
        <div class="field">
          <label class="lab" for="f-name">标题</label>
          <div class="ctrl"><input id="f-name" placeholder="显示名称" bind:value={draft.name} required /></div>
        </div>
        <div class="field">
          <label class="lab">图标</label>
          <div class="ctrl">
            <button type="button" class="iconbtn" class:on={showIconPicker}
                    onclick={() => showIconPicker = !showIconPicker} title="选择图标" aria-label="选择图标">
              {#if draft.icon && iconPaths(draft.icon)}
                <Icon name={draft.icon} size={18} />
              {:else}
                <span class="noicon">·</span>
              {/if}
            </button>
            <input aria-label="图标名" placeholder="选图标或直接输入（desktop、fas fa-server…）" value={draft.icon ?? ''}
                   onchange={(e) => setIcon((e.target as HTMLInputElement).value)} />
          </div>
        </div>
        {#if showIconPicker}
          <div class="iconpick">
            {#each ICON_NAMES as n (n)}
              <button type="button" class="cell" class:sel={(draft.icon ?? '') === n} title={n}
                      onclick={() => setIcon(n)}>
                <Icon name={n} size={18} />
              </button>
            {/each}
            <button type="button" class="cell none" class:sel={!draft.icon} title="不显示图标"
                    onclick={() => setIcon('')}>无</button>
          </div>
        {/if}
        <div class="field">
          <label class="lab" for="f-host">主机</label>
          <div class="ctrl">
            <input id="f-host" placeholder="IP 或域名" bind:value={draft.host} required />
            <input class="port" type="number" aria-label="端口" placeholder="22" bind:value={draft.port} />
          </div>
        </div>
        <div class="field">
          <label class="lab" for="f-user">用户名</label>
          <div class="ctrl"><input id="f-user" placeholder="登录账户" bind:value={draft.username} required /></div>
        </div>
        <div class="field">
          <label class="lab" for="f-proto">连接类型</label>
          <div class="ctrl">
            <select id="f-proto" value={draft.protocol} onchange={(e) => onProtocolChange((e.target as HTMLSelectElement).value)}>
              <option value="ssh">SSH 终端</option>
              <option value="rdp">RDP 远程桌面</option>
            </select>
          </div>
        </div>
        {#if draft.protocol === 'ssh'}
          <div class="field">
            <label class="lab" for="f-auth">认证方式</label>
            <div class="ctrl">
              <select id="f-auth" bind:value={draft.auth_method}>
                <option value="agent">ssh-agent</option>
                <option value="key">私钥</option>
                <option value="password">密码</option>
              </select>
            </div>
          </div>
          {#if draft.auth_method === 'key'}
            <div class="field">
              <label class="lab" for="f-key">私钥路径</label>
              <div class="ctrl"><input id="f-key" placeholder="~/.ssh/id_ed25519" bind:value={draft.key_path} /></div>
            </div>
            <p class="hint">路径可以 ~ 开头。支持 OpenSSH、PEM（PKCS#1/PKCS#8）和 PuTTY 的 .ppk；私钥带口令的，连接时会单独问一次口令，口令只在这次会话里用，不写进配置文件。</p>
          {/if}
          {#if draft.auth_method === 'password'}
            <div class="field">
              <label class="lab" for="f-pw">密码</label>
              <div class="ctrl">
                <input id="f-pw" type="password" autocomplete="new-password" bind:value={draftSecret}
                       placeholder={savedPwIds.has(draft.id) ? '已保存（留空保持不变）' : '输入密码（可选）'} />
              </div>
            </div>
          {:else if draft.auth_method === 'key'}
            <div class="field">
              <label class="lab" for="f-pass">私钥口令（可选）</label>
              <div class="ctrl">
                <input id="f-pass" type="password" autocomplete="new-password" bind:value={draftSecret}
                       placeholder={savedPwIds.has(draft.id) ? '已保存（留空保持不变）' : '输入密码（可选）'} />
              </div>
            </div>
          {/if}
          {#if draft.auth_method === 'password' || draft.auth_method === 'key'}
            <label class="ck"><input type="checkbox" bind:checked={draftSavePw} /> 保存到本机（明文）</label>
            {#if savedPwIds.has(draft.id)}
              <div class="field">
                <span class="lab"></span>
                <div class="ctrl">
                  <button type="button" onclick={() => { draftClearPw = true; draftSecret = ''; }}>清除已保存密码</button>
                </div>
              </div>
            {/if}
            <p class="hint">密码以明文存放在 data/secrets.json，仅建议在受信任设备使用；不勾选保存则下次连接会再次询问。Windows RDP 仍由系统客户端弹窗。</p>
          {/if}
        {/if}
        {#if draft.protocol === 'rdp'}
          <div class="group">
            <span class="gtitle">远程桌面（交给系统客户端 xfreerdp / mstsc）</span>
            <div class="field">
              <label class="lab" for="f-rdp-pw">密码</label>
              <div class="ctrl">
                <input id="f-rdp-pw" type="password" autocomplete="new-password" bind:value={draftSecret}
                       placeholder={savedPwIds.has(draft.id) ? '已保存（留空保持不变）' : '输入密码（可选）'} />
              </div>
            </div>
            <label class="ck"><input type="checkbox" bind:checked={draftSavePw} /> 保存到本机（明文）</label>
            {#if savedPwIds.has(draft.id)}
              <div class="field">
                <span class="lab"></span>
                <div class="ctrl">
                  <button type="button" onclick={() => { draftClearPw = true; draftSecret = ''; }}>清除已保存密码</button>
                </div>
              </div>
            {/if}
            <p class="hint">密码以明文存放在 data/secrets.json，仅建议在受信任设备使用；不勾选保存则下次连接会再次询问。Windows RDP 仍由系统客户端弹窗。</p>
            <div class="field">
              <label class="lab" for="f-sec">兼容方式</label>
              <div class="ctrl">
                <select id="f-sec" bind:value={draft.rdp.sec}>
                  <option value="auto">自动协商（Win10/11、2012R2+）</option>
                  <option value="nla">强制 NLA（域账户、2008R2+）</option>
                  <option value="weak">强制 NLA + 老 TLS（2008 R2 SHA1 证书；仅 Linux xfreerdp3 生效）</option>
                  <option value="tls">仅 TLS</option>
                  <option value="legacy">老式 RDP（2003/XP，无 NLA）</option>
                </select>
              </div>
            </div>
            <div class="field">
              <label class="lab">分辨率</label>
              <div class="ctrl">
                <select aria-label="分辨率" value={resValue} onchange={(e) => onResChange((e.target as HTMLSelectElement).value)}>
                  <option value="">默认（窗口/动态）</option>
                  {#each RES_PRESETS as r (r)}<option value={r}>{r}</option>{/each}
                  <option value="custom">自定义…</option>
                </select>
                {#if resValue === 'custom'}
                  <input class="port" type="number" min="16" max="10000" bind:value={draft.rdp.width} title="宽" aria-label="宽" />
                  <span class="x">×</span>
                  <input class="port" type="number" min="16" max="10000" bind:value={draft.rdp.height} title="高" aria-label="高" />
                {/if}
              </div>
            </div>
            <div class="field">
              <label class="lab" for="f-share">共享目录</label>
              <div class="ctrl">
                <input id="f-share" placeholder="映射到远程的本地目录（留空=不共享）" list="rdpsharelist"
                       value={draft.rdp.shareDir ?? ''}
                       onchange={(e) => { draft.rdp.shareDir = (e.target as HTMLInputElement).value.trim() || null; }} />
                <datalist id="rdpsharelist">
                  {#each shareDirHistory as d (d)}<option value={d}></option>{/each}
                </datalist>
              </div>
            </div>
            <div class="field">
              <label class="lab">选项</label>
              <div class="ctrl checks">
                <label class="chk"><input type="checkbox" bind:checked={draft.rdp.clipboard} /> 剪贴板共享</label>
                <label class="chk"><input type="checkbox" bind:checked={draft.rdp.fullscreen} /> 全屏</label>
                <label class="chk"><input type="checkbox" bind:checked={draft.rdp.ignoreCert} /> 忽略服务端证书</label>
              </div>
            </div>
          </div>
          <p class="hint">桌面在独立窗口打开，不占标签页。Linux 用 xfreerdp（apt install freerdp3-x11），目录共享是精确到这一个目录；Windows 用自带 mstsc，密码在其弹窗里输入，目录共享是整个本地驱动器（远程里是 \\tsclient\C）。选了跳转时，RDP 流量经 SSH 跳板转发，适合 3389 只对跳板机开放的场景。</p>
        {/if}
        <div class="field">
          <label class="lab" for="f-remark">备注</label>
          <div class="ctrl"><input id="f-remark" placeholder="给自己看的说明（可留空）" bind:value={draft.remark} /></div>
        </div>
        <div class="field">
          <label class="lab" for="f-group">分组</label>
          <div class="ctrl">
            <input id="f-group" placeholder="文件夹名称（留空为未分组）" list="grouplist" value={draft.group_tag ?? ''}
                   onchange={(e) => { draft.group_tag = (e.target as HTMLInputElement).value.trim() || null; }} />
            <datalist id="grouplist">
              {#each groupNames as g (g)}<option value={g}></option>{/each}
            </datalist>
          </div>
        </div>
        <div class="field">
          <label class="lab" for="f-jump">跳转</label>
          <div class="ctrl">
            <select id="f-jump" value={draft.proxy_jump ?? ''}
                    onchange={(e) => { draft.proxy_jump = (e.target as HTMLSelectElement).value || null; }}>
              <option value="">直连</option>
              {#each servers.filter((s) => s.id !== draft.id && s.protocol !== 'rdp') as s (s.id)}
                <option value={s.id}>经 {s.name}（{s.username}@{s.host}）</option>
              {/each}
            </select>
          </div>
        </div>
        <div class="fbtns">
          <button type="button" onclick={closeForm}>取消</button>
          <button type="submit">保存</button>
        </div>
      </form>
    </div>
  {/if}

  {#if pwPrompt}
    <div class="pwmask" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) answerPw(false); }}>
      <form class="pwbox" onsubmit={(e) => { e.preventDefault(); answerPw(true); }}>
        <p>{pwPrompt.msg}</p>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="password" bind:value={pwPrompt.value} use:focusEl
               onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); answerPw(false); } }} />
        <div class="row">
          <button type="submit">{pwPrompt.okLabel}</button>
          <button type="button" onclick={() => answerPw(false)}>取消</button>
        </div>
      </form>
    </div>
  {/if}
</main>

{#if locked}
  <div class="lockmask">
    <form class="lockbox" onsubmit={(e) => { e.preventDefault(); void unlockVault(); }}>
      <h2>lterm 已锁定</h2>
      <p>输入启动密码，解锁本机保存的服务器密码</p>
      <input type="password" bind:value={lockPw} use:focusEl aria-label="启动密码"
             onkeydown={(e) => { if (e.key === 'Escape') e.preventDefault(); }} />
      {#if lockErr}<p class="lockerr">{lockErr}</p>{/if}
      <button type="submit" disabled={lockBusy || !lockPw}>{lockBusy ? '解锁中…' : '解锁'}</button>
    </form>
  </div>
{/if}

<style>
  main { display: flex; height: 100vh; font-family: system-ui, sans-serif; }
  .rail { flex-shrink: 0; width: 30px; z-index: 40; border: none; border-right: 1px solid #3a3a3a; background: #202020; color: #8ab4f8; cursor: pointer; font-size: 18px; display: flex; align-items: center; justify-content: center; padding: 0; }
  .rail:hover { background: #2d3a4a; }
  main { position: relative; }
  aside { width: 300px; border-right: 1px solid #333; display: flex; flex-direction: column; background: #1e1e1e; color: #ddd; }
  .toolbar { display: flex; gap: 6px; padding: 8px; }
  /* 搜索框留了 min-width:0 才肯跟着缩，否则几个按钮就能把它挤到面板外面 */
  .search { flex: 1; min-width: 0; padding: 6px; border-radius: 6px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .toolbar button { padding: 0 10px; }
  /* 列表底部工具栏：加服务器、建分组、按名称重排都在这儿，以后往右边排就行 */
  .listbar { display: flex; gap: 6px; padding: 7px 8px; border-top: 1px solid #2a2a2a; background: #1a1a1a; }
  .listbar button { flex: 1; padding: 4px 6px; font-size: 12px; }
  .listbar button:disabled { opacity: .35; cursor: default; }
  .listbar button:disabled:hover { color: inherit; }
  .formmask { position: fixed; inset: 0; z-index: 55; background: rgba(0,0,0,.55); display: flex; align-items: flex-start; justify-content: center; padding: 40px 12px; }
  .server-form { display: flex; flex-direction: column; gap: 9px; padding: 14px 16px; width: 620px; max-width: 94vw; max-height: calc(100vh - 80px); overflow-y: auto; background: #232323; border: 1px solid #444; border-radius: 10px; box-shadow: 0 12px 40px rgba(0,0,0,.5); }
  .server-form .fhead { display: flex; justify-content: space-between; align-items: center; font-weight: 600; color: #ccc; padding-bottom: 8px; border-bottom: 1px solid #333; font-size: 14px; }
  /* 两栏：固定标签列 + 控件列，标签常驻不随输入消失 */
  .field { display: grid; grid-template-columns: 72px 1fr; align-items: center; gap: 10px; }
  .lab { color: #9aa7b4; font-size: 12px; text-align: right; }
  .ctrl { display: flex; gap: 6px; min-width: 0; }
  .ctrl > input, .ctrl > select { flex: 1; min-width: 0; }
  .ctrl.checks { flex-wrap: wrap; gap: 14px; }
  .ctrl .x { color: #777; align-self: center; }
  .server-form input, .server-form select, .server-form button { padding: 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; font-size: 13px; }
  .server-form input:focus, .server-form select:focus { outline: none; border-color: #8ab4f8; }
  .server-form option { background: #2a2a2a; color: #eee; }
  .port { width: 66px; flex: 0 0 auto !important; }
  .server-form .hint { margin: 0; font-size: 11px; color: #777; line-height: 1.5; }
  .server-form .chk { display: flex; align-items: center; gap: 5px; font-size: 12px; color: #bbb; cursor: pointer; }
  .server-form .ck { display: flex; align-items: center; gap: 5px; font-size: 12px; color: #bbb; cursor: pointer; margin-left: 82px; }
  .group { display: flex; flex-direction: column; gap: 9px; padding: 11px 12px 12px; border: 1px solid #333; border-radius: 8px; background: #1e1e1e; }
  .gtitle { color: #8ab4f8; font-size: 12px; font-weight: 600; }
  .fbtns { display: flex; justify-content: flex-end; gap: 8px; margin-top: 2px; }
  .fbtns button { padding: 6px 18px; cursor: pointer; }
  .fbtns button[type=submit] { background: #2f5d8a; border-color: #4c8bf5; color: #eaf2ff; }
  .fbtns button[type=submit]:hover { background: #386e9f; }
  .iconbtn { width: 34px; flex-shrink: 0; display: flex; align-items: center; justify-content: center; color: #8ab4f8;
    border: 1px solid #444; border-radius: 5px; background: #2a2a2a; cursor: pointer; }
  .iconbtn:hover, .iconbtn.on { border-color: #8ab4f8; }
  .iconbtn .noicon { color: #666; font-size: 16px; line-height: 1; }
  .iconpick { display: grid; grid-template-columns: repeat(8, 1fr); gap: 4px; padding: 4px; margin-left: 82px;
    border: 1px solid #3a3a3a; border-radius: 6px; background: #1c1c1c; }
  .iconpick .cell { display: flex; align-items: center; justify-content: center; height: 28px; color: #9fb6cc;
    border: 1px solid transparent; border-radius: 5px; background: #242424; cursor: pointer; font-size: 11px; }
  .iconpick .cell:hover { background: #2f2f2f; color: #ddd; }
  .iconpick .cell.sel { border-color: #8ab4f8; color: #8ab4f8; }
  .iconpick .none { grid-column: 1 / -1; color: #888; }
  .server-list { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; }
  .server-list li { display: flex; align-items: stretch; }
  .folder-head { display: flex; align-items: stretch; position: sticky; top: 0; z-index: 1; }
  .folder { flex: 1; min-width: 0; display: flex; align-items: center; gap: 6px; text-align: left; background: #1a1a1a; border: none; border-bottom: 1px solid #2a2a2a; border-top: 1px solid #2a2a2a; color: #8ab4f8; padding: 5px 10px; cursor: pointer; font-size: 12px; }
  .folder:hover { background: #232323; }
  .fold { width: 12px; color: #777; }
  .fname { font-weight: 600; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fcount { flex-shrink: 0; color: #666; font-size: 11px; }
  .folds { width: 44px; flex-shrink: 0; display: flex; align-items: stretch; justify-content: flex-end;
    background: #1a1a1a; border-top: 1px solid #2a2a2a; border-bottom: 1px solid #2a2a2a; }
  .foldact { opacity: 0; transition: opacity .12s; }
  .folder-head:hover .foldact { opacity: 1; }
  /* 行内操作按钮单独一行放条目下方，横向不再挤占名称空间，以后加按钮直接往这里排 */
  .member { flex-direction: column; align-items: stretch; }
  .member .server { padding-left: 22px; border-bottom: none; }
  /* 紧凑模式：一行一条，标题占满，行间留一条分隔线 */
  .member.compact .server { flex-direction: row; align-items: center; padding: 4px 10px 4px 22px; }
  .member.compact .titlerow { flex: 1; min-width: 0; }
  .member.compact .name { flex: 1; min-width: 0; }
  .member.compact { border-bottom: 1px solid #2a2a2a; }
  .member:hover .acts { background: #2d2d2d; }
  .acts { display: flex; justify-content: flex-start; gap: 2px; padding: 0 6px 3px 16px; border-bottom: 1px solid #2a2a2a; }
  .acts button { padding: 0 6px; line-height: 1.5; display: flex; align-items: center; justify-content: center; }
  .acts button:disabled { opacity: .3; cursor: default; }
  .acts button:disabled:hover { color: inherit; }
  /* 拖动手柄：负边距挪进 .server 的左内边距里，不挤名称位置 */
  .grip { flex-shrink: 0; width: 12px; margin-left: -12px; display: flex; align-items: center;
    color: #555; font-size: 12px; cursor: grab; touch-action: none; }
  .grip:hover { color: #8ab4f8; }
  .server-list.dragging { user-select: none; }
  .server-list.dragging .grip { cursor: grabbing; }
  .member.dragging .server { opacity: .45; }
  .member.drop-top { box-shadow: inset 0 2px 0 0 #4fd6be; }
  .member.drop-bottom { box-shadow: inset 0 -2px 0 0 #4fd6be; }
  .member .server { padding-left: 22px; }
  .server { flex: 1; min-width: 0; text-align: left; background: none; border: none; border-bottom: 1px solid #2a2a2a; color: #ddd; padding: 8px 10px; cursor: pointer; display: flex; flex-direction: column; gap: 2px; }
  .server:hover { background: #2d2d2d; }
  .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .titlerow { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .sicon { display: flex; align-items: center; color: #8ab4f8; flex-shrink: 0; }
  .meta { font-size: 12px; color: #888; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .remark { font-size: 12px; color: #6a9fb5; }
  .del { border: none; background: none; color: #666; cursor: pointer; }
  .del:hover { color: #e57373; }
  .empty { padding: 16px; color: #666; }
  .right { flex: 1; display: flex; flex-direction: column; background: #101010; min-width: 0; }
  .tabbar { display: flex; background: #181818; border-bottom: 1px solid #2a2a2a; overflow-x: auto; }
  .tabbar::-webkit-scrollbar { display: none; }
  .tab { display: flex; align-items: center; gap: 6px; padding: 6px 12px; background: none; border: none; border-right: 1px solid #262626; color: #999; cursor: pointer; font-size: 13px; white-space: nowrap; }
  .tab.active { background: #101010; color: #eee; box-shadow: inset 0 3px 0 0 #4fd6be; }
  .tab .close { border: none; background: none; color: inherit; padding: 0 2px; border-radius: 3px; cursor: pointer; line-height: 1; }
  .tab .close:hover { background: #444; color: #fff; }
  .newtab { color: #6a9fb5; }
  .panes { flex: 1; min-height: 0; position: relative; display: flex; }
  .panes > :global(div:not(.welcome)) { position: absolute; inset: 0; }
  .welcome { margin: auto; text-align: center; color: #777; }
  .welcome h2 { color: #aaa; margin-bottom: 4px; }
  .welcome .kbd { font-size: 12px; color: #555; }
  .errtoast { position: absolute; right: 10px; bottom: 36px; z-index: 30; max-width: 72%; display: flex; align-items: flex-start; gap: 8px;
    padding: 7px 10px; background: #4a1d1d; color: #f0a0a0; font-size: 12px; border: 1px solid #6b2b2b; border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0,0,0,.55); }
  .errtoast span { word-break: break-all; }
  .updatetoast { position: absolute; left: 10px; bottom: 36px; z-index: 30; max-width: 76%; display: flex; align-items: center; gap: 8px;
    padding: 7px 10px; background: #16324c; color: #cfe4ff; font-size: 12px; border: 1px solid #2b5480; border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0,0,0,.55); }
  .updatetoast .uttitle { font-weight: 600; white-space: nowrap; }
  .updatetoast .utsub { color: #9fc4e8; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .updatetoast .utact { padding: 2px 8px; white-space: nowrap; }
  .updatetoast .dismiss { color: #cfe4ff; }
  .setdlg.upddlg { width: 560px; }
  .setdlg .updmeta { font-size: 12px; color: #9fc4e8; }
  .setdlg .updhint { padding: 6px 8px; font-size: 12px; color: #e6c07b; background: #3a2f1b; border: 1px solid #5c4a26; border-radius: 6px; }
  .setdlg .updnotes { margin: 0; max-height: 220px; overflow-y: auto; padding: 8px 10px; background: #1b1b1b; border: 1px solid #383838; border-radius: 6px;
    color: #bbb; font-family: inherit; font-size: 12px; line-height: 1.55; white-space: pre-wrap; word-break: break-word; }
  .setdlg .updbar { height: 6px; overflow: hidden; background: #1b1b1b; border: 1px solid #383838; border-radius: 3px; }
  .setdlg .updbar span { display: block; height: 100%; background: linear-gradient(90deg, #2f6ea5, #4fd6be); }
  .setdlg .updacts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .setdlg .updacts .spacer { flex: 1; }
  .setdlg .updacts button:disabled { opacity: .5; cursor: default; }
  .setmask.updmask { z-index: 57; }
  .setdlg .supd { font-size: 12px; color: #9fc4e8; margin: 2px 0 4px; }
  .dismiss { background: none; border: none; color: #f0a0a0; cursor: pointer; flex-shrink: 0; }
  .status { padding: 4px 10px; font-size: 12px; color: #999; border-top: 1px solid #333; display: flex; gap: 10px; align-items: center; position: relative; }
  .status button { padding: 2px 10px; }
  .status .flash { color: #4fd6be; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .spacer { flex: 1; }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #555; }
  .dot.connected { background: #4caf50; }
  .dot.closed { background: #e6a23c; }
  .gear { background: none; border: none; color: #999; cursor: pointer; font-size: 12px; }
  .icon { border: none; background: none; color: #666; cursor: pointer; padding: 0 6px; }
  .icon:hover { color: #8ab4f8; }
  .setmask { position: fixed; inset: 0; z-index: 56; background: rgba(0,0,0,.55); display: flex; align-items: center; justify-content: center; padding: 24px 12px; }
  .setdlg { width: 640px; max-width: 94vw; max-height: calc(100vh - 48px); overflow-y: auto; display: flex; flex-direction: column; gap: 9px;
    padding: 14px 16px; background: #232323; border: 1px solid #444; border-radius: 10px; color: #ccc;
    box-shadow: 0 12px 40px rgba(0,0,0,.5); font-size: 13px; }
  .setdlg .shead { display: flex; justify-content: space-between; align-items: center; font-weight: 600; font-size: 14px; color: #ddd;
    padding-bottom: 8px; border-bottom: 1px solid #333; }
  .setdlg .shead .sclose { border: none; background: none; color: #999; font-size: 14px; cursor: pointer; padding: 2px 6px; border-radius: 4px; }
  .setdlg .shead .sclose:hover { background: #444; color: #fff; }
  .setdlg .sec { margin-top: 6px; font-size: 12px; color: #8ab4f8; border-bottom: 1px solid #2f2f2f; padding-bottom: 3px; }
  .setdlg .srow { display: flex; gap: 8px; align-items: center; }
  .setdlg .lbl { width: 82px; flex-shrink: 0; color: #aaa; }
  .setdlg .ctlsel { flex: 1; min-width: 0; padding: 5px 6px; font-size: 13px; }
  .setdlg .fontin { flex: 1; min-width: 0; padding: 5px 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; font-size: 13px; }
  .setdlg .ck { display: flex; gap: 8px; align-items: flex-start; font-size: 13px; color: #ccc; cursor: pointer; }
  .setdlg .hint { margin: 0; font-size: 11px; color: #777; line-height: 1.5; }
  .setdlg button { padding: 4px 10px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #ddd; cursor: pointer; }
  .setdlg .spath { margin-top: 8px; padding-top: 8px; border-top: 1px solid #2f2f2f; font-size: 11px; color: #888; }
  .setdlg .spath code { color: #9fd0ff; word-break: break-all; user-select: text; }
  .srvctx { position: fixed; z-index: 70; display: flex; flex-direction: column; padding: 3px; min-width: 132px;
    background: #242424; border: 1px solid #4a4a4a; border-radius: 8px; box-shadow: 0 10px 28px rgba(0,0,0,.6); }
  .srvctx .ctxi { text-align: left; padding: 5px 12px; font-size: 13px; border: none; background: none; color: #ddd; cursor: pointer; border-radius: 5px; }
  .srvctx .ctxi:hover { background: #2f5063; color: #fff; }
  .srvctx .ctxi.danger { color: #e58383; }
  .srvctx .ctxi.danger:hover { background: #4a1d1d; color: #f0a0a0; }
  .srvctx .ctxsep { height: 1px; margin: 3px 6px; background: #3a3a3a; }
  .setdlg .numin { width: 64px; padding: 4px 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; font-size: 13px; }
  .setdlg .unit { color: #888; font-size: 11px; }
  .srow { display: flex; gap: 8px; align-items: center; white-space: nowrap; flex-wrap: wrap; }
  .srow .lbl { width: auto; min-width: 30px; color: #aaa; flex-shrink: 0; }
  .srow .ctlsel { flex: 1; min-width: 0; max-width: 100%; padding: 3px 4px; font-size: 12px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .srow .ctlsel option { background: #2a2a2a; color: #eee; }
  .srow .num { width: 26px; text-align: center; }
  .srow .num.ver { width: auto; text-align: left; }
  .srow .fontin { flex: 1; min-width: 100px; padding: 4px 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .pwmask { position: fixed; inset: 0; z-index: 60; background: rgba(0,0,0,.55); display: flex; align-items: center; justify-content: center; }
  .pwbox { background: #252525; border: 1px solid #444; border-radius: 8px; padding: 14px 16px; width: 340px; color: #ccc; display: flex; flex-direction: column; gap: 10px; }
  .pwbox p { margin: 0; font-size: 13px; word-break: break-all; }
  .pwbox input { padding: 6px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .pwbox .row { display: flex; gap: 6px; justify-content: flex-end; }
  .pwbox button { padding: 5px 12px; border-radius: 5px; border: 1px solid #444; background: #2a2a2a; color: #eee; cursor: pointer; }
  .lockmask { position: fixed; inset: 0; z-index: 200; background: #1a1a1a; display: flex; align-items: center; justify-content: center; }
  .lockbox { width: 360px; background: #252525; border: 1px solid #444; border-radius: 10px; padding: 20px; color: #ddd; display: flex; flex-direction: column; gap: 12px; }
  .lockbox h2 { margin: 0; font-size: 16px; font-weight: 600; }
  .lockbox p { margin: 0; font-size: 12px; color: #999; }
  .lockbox input { padding: 8px; border-radius: 6px; border: 1px solid #444; background: #2a2a2a; color: #eee; }
  .lockbox button { padding: 8px; border-radius: 6px; border: 1px solid #3a6ea5; background: #2d4a6a; color: #eaf2ff; cursor: pointer; }
  .lockbox button:disabled { opacity: .5; cursor: default; }
  .lockerr { color: #e06c6c !important; }
  .focuswarn { color: #e6a23c; font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .connecting { color: #e6a23c; font-size: 12px; display: flex; align-items: center; gap: 6px; white-space: nowrap; }
  .spin { flex-shrink: 0; width: 10px; height: 10px; border-radius: 50%; border: 2px solid #e6a23c55; border-top-color: #e6a23c;
    animation: lterm-spin .8s linear infinite; }
  @keyframes lterm-spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spin { animation-duration: 2.4s; } }
  .server.busy { background: #232c38; cursor: progress; }
  .server .ing { color: #e6a23c; font-size: 12px; font-weight: 400; flex-shrink: 0; }
  .server:focus-visible, .folder:focus-visible, .icon:focus-visible, .del:focus-visible,
  .search:focus-visible, .toolbar button:focus-visible, .listbar button:focus-visible, .tab:focus-visible {
    outline: 2px solid #8ab4f8; outline-offset: -2px;
  }
  /* 终端拿到焦点时描一圈绿边，一眼能看出“现在敲的字会进这里” */
  .panes :global(.xterm.xterm-focus) { box-shadow: inset 0 0 0 2px rgba(79,214,190,.45); }
</style>
