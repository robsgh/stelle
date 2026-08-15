<script lang="ts">
  import { onMount } from 'svelte';
  import CircleAlert from '@lucide/svelte/icons/circle-alert';
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import RefreshCw from '@lucide/svelte/icons/refresh-cw';
  import Favicon from '$lib/Favicon.svelte';
  import type { Dashboard, LinkWidget, LuaWidget, WidgetState } from '$lib/types';
  import '../app.css';

  let dashboard: Dashboard | null = null;
  let dashboardError = '';
  let widgetStates: Record<string, WidgetState> = {};
  let now: Date | null = null;

  onMount(() => {
    now = new Date();
    const clock = window.setInterval(() => now = new Date(), 1_000);
    return () => window.clearInterval(clock);
  });

  onMount(() => {
    let cancelled = false;

    async function loadDashboard() {
      try {
        const response = await fetch('/api/dashboard');
        if (!response.ok) throw new Error('Dashboard configuration is unavailable');
        const loaded: Dashboard = await response.json();
        if (cancelled) return;
        dashboard = loaded;
        document.documentElement.dataset.theme = loaded.theme;
        document.documentElement.style.setProperty('--accent', loaded.accent);
        for (const widget of loaded.widgets) {
          if (widget.type !== 'lua') continue;
          refresh(widget, false, true);
        }
      } catch (error) {
        if (!cancelled) {
          dashboardError = error instanceof Error ? error.message : 'Could not load the dashboard';
        }
      }
    }

    loadDashboard();
    return () => { cancelled = true; };
  });

  async function refresh(widget: LuaWidget, force = false, showLoading = false) {
    const previous = widgetStates[widget.id];
    if (showLoading || !previous) {
      widgetStates = { ...widgetStates, [widget.id]: { status: 'loading' } };
    }
    try {
      const endpoint = `/api/widgets/${encodeURIComponent(widget.id)}${force ? '/refresh' : ''}`;
      const response = await fetch(endpoint, { method: force ? 'POST' : 'GET' });
      const body = await response.json();
      if (!response.ok) throw new Error(body?.error?.message ?? 'The widget could not be refreshed');
      widgetStates = { ...widgetStates, [widget.id]: { status: 'ready', content: body.content } };
    } catch (error) {
      if (!force && previous?.status === 'ready') return;
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

  function greeting(date: Date): string {
    const hour = date.getHours();
    if (hour < 12) return 'Good morning';
    if (hour < 18) return 'Good afternoon';
    return 'Good evening';
  }

  function currentTime(date: Date): string {
    return date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  }

  function defaultTitle(date: Date | null): string {
    return date ? greeting(date) : 'Welcome';
  }

  function defaultSubtitle(date: Date | null): string {
    return date ? currentTime(date) : '';
  }

  function gridColumns(cardCount: number): number {
    return Math.max(1, Math.ceil(Math.sqrt(cardCount)));
  }

  function gridMaxWidth(cardCount: number): number {
    const columns = gridColumns(cardCount);
    const cardWidth = 350;
    const gap = 18;
    return columns * cardWidth + (columns - 1) * gap;
  }

  function links(dashboard: Dashboard): LinkWidget[] {
    return dashboard.widgets.filter((widget): widget is LinkWidget => widget.type === 'link');
  }

  function smartWidgets(dashboard: Dashboard): LuaWidget[] {
    return dashboard.widgets.filter((widget): widget is LuaWidget => widget.type === 'lua');
  }

  function metricParts(value: string | number | boolean | null): { value: string; unit: string } {
    const text = value === null ? '—' : String(value);
    const match = text.match(/^([+-]?\d+(?:\.\d+)?)\s*(%|°|[A-Za-z]+)$/);
    return match ? { value: match[1], unit: match[2] } : { value: text, unit: '' };
  }
</script>

<svelte:head><title>{dashboard?.title ?? defaultTitle(now)}</title></svelte:head>

<main class="shell">
  <header class="masthead">
    <div class="brandmark" aria-hidden="true">S</div>
    <div class="heading">
      <h1>{dashboard?.title ?? defaultTitle(now)}</h1>
      <p>{dashboard?.subtitle ?? defaultSubtitle(now)}</p>
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
    {@const linkWidgets = links(dashboard)}
    {@const dataWidgets = smartWidgets(dashboard)}
    <div
      class="dashboard-content"
      style={`--dashboard-max-width:${gridMaxWidth(linkWidgets.length || dataWidgets.length)}px`}
    >
      {#if linkWidgets.length > 0}
        <section
          class="widget-grid"
          aria-label="Links"
          style={`--grid-columns:${gridColumns(linkWidgets.length)}`}
        >
          {#each linkWidgets as widget}
          <a
            class="card link-card"
            href={widget.url}
            style={`--widget-accent:${widget.accent ?? dashboard.accent}`}
          >
            <Favicon url={widget.url} />
            <span class="link-copy">
              <strong>{widget.label}</strong>
              <small>{widget.description}</small>
              <span class="hostname">{new URL(widget.url).host}</span>
            </span>
          </a>
          {/each}
        </section>
      {/if}

      {#if dataWidgets.length > 0}
        <section class="telemetry-section" aria-labelledby="telemetry-heading">
          <h2 id="telemetry-heading">Telemetry</h2>
          <div class="smart-stack">
          {#each dataWidgets as widget}
          {@const state = widgetStates[widget.id] ?? { status: 'loading' }}
          <article class="card stats-card smart-card">
            {#if state.status === 'loading'}
              <div class="card-top">
                <div>
                  <span class="skeleton title-skeleton"></span><span class="skeleton text-skeleton"></span>
                </div>
                <LoaderCircle class="spinner" size={21} aria-label="Refreshing" />
              </div>
              <div class="metrics loading-metrics"><span></span><span></span><span></span></div>
            {:else if state.status === 'error'}
              <div class="error-state" role="alert">
                <span class="error-badge"><CircleAlert size={18} /></span>
                <div><strong>Widget unavailable</strong><p>{state.message}</p></div>
              </div>
              <button class="refresh-button" onclick={() => refresh(widget, true, true)}><RefreshCw size={17} /> Retry</button>
            {:else}
              <div class="card-top">
                <div>
                  {#if state.content.href}
                    <a class="widget-title" href={state.content.href}>{state.content.title}</a>
                  {:else}<h2 class="widget-title">{state.content.title}</h2>{/if}
                  <p class="widget-subtitle">{state.content.subtitle}</p>
                </div>
                <div class="refresh-control">
                  <span class="updated-at">Updated {displayTime(state.content.fetched_at)}</span>
                  <button
                    class="icon-button"
                    onclick={() => refresh(widget, true, true)}
                    aria-label={`Refresh ${state.content.title}. Updated at ${displayTime(state.content.fetched_at)}`}
                  >
                    <RefreshCw size={18} />
                  </button>
                </div>
              </div>
              <div class={`metrics metrics-${state.content.metrics.length}`}>
                {#each state.content.metrics as metric}
                  {@const parts = metricParts(metric.value)}
                  <div>
                    <strong>{parts.value}{#if parts.unit}<small>{parts.unit}</small>{/if}</strong>
                    <span>{metric.label}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </article>
          {/each}
          </div>
        </section>
      {/if}
    </div>
  {/if}

</main>
