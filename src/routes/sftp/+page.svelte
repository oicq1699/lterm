<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import SftpBrowser from '$lib/SftpBrowser.svelte';
  type Boot = { sid: string; profileId: string };
  const boot = (window as any).__LTERM_SFTP__ as Boot | undefined;
  let histMax = $state(8);
  let confirmDelete = $state(true);
  let confirmOverwrite = $state(false);
  onMount(async () => {
    try {
      const s = await invoke<any>('get_settings');
      histMax = Math.min(30, Math.max(3, s.sftpHist || 8));
      confirmDelete = s.confirmSftpDelete !== false;
      confirmOverwrite = s.confirmSftpOverwrite === true;
    } catch {}
  });
</script>
{#if boot}
  <div class="win"><SftpBrowser sid={boot.sid} profileId={boot.profileId} password={null}
        preAuthenticated={true} wide={true} {histMax} {confirmDelete} {confirmOverwrite} /></div>
{:else}
  <div class="bad">缺少窗口参数</div>
{/if}

<style>
  :global(html), :global(body) { margin: 0; height: 100%; background: #1e1e1e; }
  .win { height: 100vh; display: flex; }
  .bad { padding: 16px; color: #e5a05a; }
</style>
