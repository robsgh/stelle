<script lang="ts">
  import { onMount } from 'svelte';
  import CircleAlert from '@lucide/svelte/icons/circle-alert';
  import ExternalLink from '@lucide/svelte/icons/external-link';
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import RefreshCw from '@lucide/svelte/icons/refresh-cw';
  import Favicon from '$lib/Favicon.svelte';
  import type { Dashboard, LuaWidget, WidgetState } from '$lib/types';
  import '../app.css';

  let dashboard: Dashboard | null = null;
  let dashboardError = '';
  let widgetStates: Record<string, WidgetState> = {};

  onMount(async () => {
    try {
      const response = await fetch('/api/dashboard');
      if (!response.ok) throw new Error('Dashboard configuration is unavailable');
      const loaded: Dashboard = await response.json();
      dashboard = loaded;
      document.documentElement.dataset.theme = loaded.theme;
      document.documentElement.style.setProperty('--accent', loaded.accent);
      for (const widget of loaded.widgets) {
        if (widget.type === 'lua') refresh(widget);
      }
    } catch (error) {
      dashboardError = error instanceof Error ? error.message : 'Could not load the dashboard';
    }
  });

  async function refresh(widget: LuaWidget) {
    widgetStates = { ...widgetStates, [widget.id]: { status: 'loading' } };
    try {
      const response = await fetch(`/api/widgets/${encodeURIComponent(widget.id)}/refresh`, { method: 'POST' });
      const body = await response.json();
      if (!response.ok) throw new Error(body?.error?.message ?? 'The widget could not be refreshed');
      widgetStates = { ...widgetStates, [widget.id]: { status: 'ready', content: body.content } };
    } catch (error) {
      widgetStates = {
        ...widgetStates,
        [widget.id]: { status: 'error', message: error instanceof Error ? error.message : 'Refresh failed' }
      };
    }
  }

  function displayTime(value: string): string {
    const date = new Date(value);
    return Number.isNaN(date.valueOf()) ? value : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<svelte:head><title>{dashboard?.title ?? 'Stelle'}</title></svelte:head>

<main class="shell">
  <header class="masthead">
    <div class="brandmark" aria-hidden="true">S</div>
    <div class="heading">
      <h1>{dashboard?.title ?? 'Stelle'}</h1>
      <p>{dashboard?.subtitle ?? 'Your homelab, at a glance.'}</p>
    </div>
  </header>

  {#if dashboardError}
    <section class="fatal" role="alert">
      <h2>Dashboard unavailable</h2>
      <p>{dashboardError}</p>
      <button onclick={() => location.reload()}>Try again</button>
    </section>
  {:else if !dashboard}
    <section class="loading-dashboard" aria-label="Loading dashboard">
      <div></div><div></div><div></div>
    </section>
  {:else}
    <section class="widget-grid" aria-label="Dashboard widgets">
      {#each dashboard.widgets as widget}
        {#if widget.type === 'link'}
          <a
            class="card link-card"
            href={widget.url}
            target="_blank"
            rel="noopener noreferrer"
            style={`--widget-accent:${widget.accent ?? dashboard.accent}`}
          >
            <Favicon url={widget.url} />
            <span class="link-copy">
              <strong>{widget.label}</strong>
              <small>{widget.description}</small>
              <span class="hostname">{new URL(widget.url).host}</span>
            </span>
            <span class="open-icon"><ExternalLink size={20} /></span>
          </a>
        {:else}
          {@const state = widgetStates[widget.id] ?? { status: 'loading' }}
          <article class="card stats-card">
            {#if state.status === 'loading'}
              <div class="card-top">
                <div><span class="skeleton title-skeleton"></span><span class="skeleton text-skeleton"></span></div>
                <LoaderCircle class="spinner" size={21} aria-label="Refreshing" />
              </div>
              <div class="metrics loading-metrics"><span></span><span></span><span></span></div>
            {:else if state.status === 'error'}
              <div class="error-state" role="alert">
                <span class="error-badge"><CircleAlert size={18} /></span>
                <div><strong>Widget unavailable</strong><p>{state.message}</p></div>
              </div>
              <button class="refresh-button" onclick={() => refresh(widget)}><RefreshCw size={17} /> Retry</button>
            {:else}
              <div class="card-top">
                <div>
                  {#if state.content.href}
                    <a class="widget-title" href={state.content.href} target="_blank" rel="noopener noreferrer">{state.content.title}</a>
                  {:else}<h2 class="widget-title">{state.content.title}</h2>{/if}
                  <p class="widget-subtitle">{state.content.subtitle}</p>
                </div>
                <div class="refresh-control">
                  <button
                    class="icon-button"
                    onclick={() => refresh(widget)}
                    aria-label={`Refresh ${state.content.title}. Updated at ${displayTime(state.content.fetched_at)}`}
                  >
                    <RefreshCw size={18} />
                  </button>
                  <span class="refresh-tooltip" role="tooltip">Updated at {displayTime(state.content.fetched_at)}</span>
                </div>
              </div>
              <div class="metrics">
                {#each state.content.metrics as metric}
                  <div><strong>{metric.value ?? '—'}</strong><span>{metric.label}</span></div>
                {/each}
              </div>
            {/if}
          </article>
        {/if}
      {/each}
    </section>
  {/if}

</main>
