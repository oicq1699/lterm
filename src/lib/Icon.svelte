<script lang="ts">
  import { iconPaths } from './icons';

  let { name, size = 16, className = '' }: { name: string | null; size?: number; className?: string } = $props();
  const paths = $derived(name ? iconPaths(name) : null);
</script>

{#if paths}
  <svg class={className} width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{@html paths}</svg>
{/if}
