<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { escapeHtml, highlight, languageFor } from './highlight';

  type Props = { sid: string; path: string; name: string; nonce?: number };
  let { sid, path, name, nonce = 0 }: Props = $props();

  type Payload = {
    kind: 'text' | 'image' | 'binary' | 'too_large';
    data: string;
    mime: string | null;
    size: number | null;
    mtime: number;
    mode: number | null;
    truncated: boolean;
    encoding_hint: string | null;
  };

  let payload = $state<Payload | null>(null);
  let loading = $state(false);
  let error = $state('');
  let zoom = $state<'fit' | 'actual'>('fit');
  let imgFailed = $state(false);
  let copied = $state(false);
  let seq = 0;

  function b64Bytes(s: string): Uint8Array {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  function decodeText(bytes: Uint8Array, hint: string | null): string {
    const tryDecode = (label: string, fatal = false): string | null => {
      try { return new TextDecoder(label, { fatal }).decode(bytes); } catch { return null; }
    };
    if (hint) {
      const s = tryDecode(hint);
      if (s !== null) return s;
    }
    if (bytes[0] === 0xff && bytes[1] === 0xfe) {
      const s = tryDecode('utf-16le');
      if (s !== null) return s;
    }
    if (bytes[0] === 0xfe && bytes[1] === 0xff) {
      const s = tryDecode('utf-16be');
      if (s !== null) return s;
    }
    const utf8 = tryDecode('utf-8', true);
    if (utf8 !== null) return utf8;
    const gbk = tryDecode('gbk');
    if (gbk !== null) return gbk;
    return new TextDecoder().decode(bytes);
  }

  async function load(p: string) {
    if (!p) return;
    const my = ++seq;
    loading = true;
    error = '';
    payload = null;
    imgFailed = false;
    zoom = 'fit';
    try {
      const r = await invoke<Payload>('sftp_preview', { sid, path: p });
      if (my !== seq) return;
      payload = r;
    } catch (e) {
      if (my !== seq) return;
      error = String(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  $effect(() => {
    const p = path;
    void nonce;
    if (p) void load(p);
  });

  const text = $derived(payload?.kind === 'text' ? decodeText(b64Bytes(payload.data), payload.encoding_hint) : '');

  function countLines(s: string): number {
    if (!s) return 0;
    let n = 1;
    for (let i = s.indexOf('\n'); i !== -1; i = s.indexOf('\n', i + 1)) n++;
    return n;
  }

  const lineCount = $derived(countLines(text));
  const tooBigToHighlight = $derived(text.length > 256 * 1024 || lineCount > 20000);
  const codeHtml = $derived(
    !text ? '' : tooBigToHighlight ? escapeHtml(text) : (highlight(text, path) ?? escapeHtml(text)),
  );
  const gutter = $derived.by(() => {
    if (lineCount <= 0) return '';
    const parts = new Array<string>(lineCount);
    for (let i = 1; i <= lineCount; i++) parts[i - 1] = String(i);
    return parts.join('\n');
  });
  // 十多万行时行号列表本身就会拖垮渲染，直接关掉行号
  const showGutter = $derived(lineCount > 0 && lineCount <= 50000);

  const fmtSize = (n: number | null) => n == null ? '—'
    : n >= 1073741824 ? (n / 1073741824).toFixed(2) + ' G'
    : n >= 1048576 ? (n / 1048576).toFixed(1) + ' M'
    : n >= 1024 ? (n / 1024).toFixed(0) + ' K' : `${n} B`;
  const fmtDate = (t: number) => t ? new Date(t * 1000).toLocaleString('zh-CN') : '—';
  const fmtMode = (m: number | null) => m == null ? '—' : '0' + (m & 0o7777).toString(8).padStart(3, '0');

  async function copyPath() {
    try {
      await invoke('clipboard_write', { text: path });
      copied = true;
      setTimeout(() => { copied = false; }, 1200);
    } catch { /* 忽略 */ }
  }
</script>

{#if path}
  <div class="head">
    <div class="titles">
      <div class="hname" title={name}>{name}</div>
      <div class="hpath" title={path}>{path}</div>
    </div>
    {#if payload}
      <div class="chips">
        <span>{fmtSize(payload.size)}</span>
        <span>{fmtDate(payload.mtime)}</span>
        <span>{fmtMode(payload.mode)}</span>
        {#if payload.kind === 'text' && languageFor(path)}<span>{languageFor(path)}</span>{/if}
      </div>
    {/if}
    <div class="ops">
      {#if payload?.kind === 'image'}
        <button onclick={() => (zoom = zoom === 'fit' ? 'actual' : 'fit')}>
          {zoom === 'fit' ? '原始尺寸' : '适应窗口'}
        </button>
      {/if}
      <button onclick={() => void copyPath()}>{copied ? '已复制' : '复制路径'}</button>
      <button onclick={() => void load(path)} title="重新读取">⟳</button>
    </div>
  </div>

  <div class="body">
    {#if loading}
      <div class="msg">加载中…</div>
    {:else if error}
      <div class="msg err">{error}</div>
    {:else if payload?.kind === 'binary'}
      <div class="msg">二进制文件，无法预览<span class="sub">{fmtSize(payload.size)}</span></div>
    {:else if payload?.kind === 'too_large'}
      <div class="msg">图片超过 8 MiB，无法预览<span class="sub">{fmtSize(payload.size)}</span></div>
    {:else if payload?.kind === 'image'}
      <div class="imgwrap" class:actual={zoom === 'actual'}>
        {#if imgFailed}
          <div class="msg">图片无法解码<span class="sub">{payload.mime ?? ''}</span></div>
        {:else}
          <img src={`data:${payload.mime};base64,${payload.data}`} alt={name} onerror={() => (imgFailed = true)} />
        {/if}
      </div>
    {:else if payload?.kind === 'text'}
      <div class="codewrap">
        {#if payload.truncated}
          <div class="banner">文件较大，仅显示前 1 MiB（共 {fmtSize(payload.size)}）</div>
        {:else if tooBigToHighlight}
          <div class="banner">文件较大，已关闭语法高亮</div>
        {/if}
        {#if text.length === 0}
          <div class="msg">空文件</div>
        {:else}
          <div class="codescroll">
            {#if showGutter}<pre class="gutter" aria-hidden="true">{gutter}</pre>{/if}
            <pre class="code"><code class="hljs">{@html codeHtml}</code></pre>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{:else}
  <div class="msg empty">选择左侧文件预览</div>
{/if}

<style>
  .head { display: flex; align-items: center; gap: 12px; padding: 8px 12px; background: #161616; border-bottom: 1px solid #2a2a2a; flex-shrink: 0; }
  .titles { min-width: 0; flex: 1; }
  .hname { color: #eee; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .hpath { color: #777; font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; margin-top: 1px; }
  .chips { display: flex; gap: 10px; color: #888; font-size: 12px; flex-shrink: 0; }
  .ops { display: flex; gap: 6px; flex-shrink: 0; }
  .ops button { background: #232323; border: 1px solid #3a3a3a; color: #bbb; border-radius: 5px; padding: 3px 9px; font-size: 12px; cursor: pointer; }
  .ops button:hover { border-color: #8ab4f8; color: #8ab4f8; }
  .body { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .msg { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; color: #888; font-size: 13px; }
  .msg.err { color: #e57373; }
  .msg.empty { height: 100%; }
  .sub { color: #666; font-size: 12px; }
  .imgwrap { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; overflow: auto; padding: 12px; }
  .imgwrap.actual { align-items: flex-start; justify-content: flex-start; }
  .imgwrap img { max-width: 100%; max-height: 100%; object-fit: contain; }
  .imgwrap.actual img { max-width: none; max-height: none; }
  .codewrap { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .banner { padding: 4px 12px; background: #3a2f18; color: #e6c07b; font-size: 12px; border-bottom: 1px solid #4a3c20; }
  .codescroll { flex: 1; min-height: 0; overflow: auto; display: flex; align-items: flex-start;
    font-family: ui-monospace, "JetBrains Mono", "Sarasa Mono SC", "Microsoft YaHei Mono", monospace; font-size: 12.5px; line-height: 18px; }
  .gutter { position: sticky; left: 0; margin: 0; padding: 8px 8px 8px 12px; text-align: right; color: #666;
    background: #1a1a1a; border-right: 1px solid #2a2a2a; user-select: none; white-space: pre; }
  .code { margin: 0; padding: 8px 16px; white-space: pre; }
  .code code { font: inherit; }
  :global(.hljs) { background: transparent !important; padding: 0 !important; overflow: visible !important; }
</style>