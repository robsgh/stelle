<script lang="ts">
  let { name, size = 24 }: { name: string; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === 'play'}
    <path d="M8 5.5v13l10-6.5z" fill="currentColor" stroke="none" />
  {:else if name === 'server'}
    <rect x="4" y="4" width="16" height="6" rx="2" />
    <rect x="4" y="14" width="16" height="6" rx="2" />
    <path d="M8 7h.01M8 17h.01M12 7h5M12 17h5" />
  {:else if name === 'refresh'}
    <path d="M20 7v5h-5M4 17v-5h5" />
    <path d="M6.1 9A7 7 0 0 1 18 6l2 2M17.9 15A7 7 0 0 1 6 18l-2-2" />
  {:else if name === 'arrow'}
    <path d="M7 17 17 7M8 7h9v9" />
  {:else}
    <path d="M12 3 9.8 9.8 3 12l6.8 2.2L12 21l2.2-6.8L21 12l-6.8-2.2z" />
  {/if}
</svg>
