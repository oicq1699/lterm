<script lang="ts">
  import RemoteFileBrowser from '$lib/RemoteFileBrowser.svelte';

  type Boot = { sid: string; profileId: string };
  const boot = (window as any).__LTERM_FILES__ as Boot | undefined;
</script>

{#if boot}
  <RemoteFileBrowser sid={boot.sid} />
{:else}
  <div class="bad">缺少窗口参数</div>
{/if}

<style>
  :global(html), :global(body) { margin: 0; height: 100%; background: #1e1e1e; }
  .bad { padding: 16px; color: #e5a05a; }
</style>