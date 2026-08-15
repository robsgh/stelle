<script lang="ts">
  let { url, size = 28 }: { url: string; size?: number } = $props();
  let failed = $state(false);
</script>

{#if !failed}
  <span class="icon-wrap">
    <img
      src={`/api/favicon?url=${encodeURIComponent(url)}`}
      alt=""
      width={size}
      height={size}
      aria-hidden="true"
      decoding="async"
      referrerpolicy="no-referrer"
      onerror={() => failed = true}
    />
  </span>
{/if}

<style>
  .icon-wrap {
    width: 44px;
    height: 44px;
    display: grid;
    flex: none;
    place-items: center;
    border-radius: 12px;
    background: var(--surface-soft);
  }

  img {
    border-radius: 6px;
    object-fit: contain;
  }
</style>
