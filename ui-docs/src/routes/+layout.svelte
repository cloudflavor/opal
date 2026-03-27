<script lang="ts">
  import { browser } from '$app/environment';
  import { page } from '$app/state';

  let { data, children } = $props();
  let query = $state('');
  let theme = $state<'dark' | 'light'>('dark');

  const filteredDocs = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    return data.docs.filter((doc) => {
      if (!needle) return true;
      return (
        doc.title.toLowerCase().includes(needle) ||
        doc.summary.toLowerCase().includes(needle) ||
        doc.slug.toLowerCase().includes(needle)
      );
    });
  });

  function hrefFor(slug: string) {
    return slug === 'index' ? '/' : `/docs/${slug}`;
  }

  function applyTheme(nextTheme: 'dark' | 'light') {
    theme = nextTheme;
    if (browser) {
      document.documentElement.dataset.theme = nextTheme;
      localStorage.setItem('opal-docs-theme', nextTheme);
    }
  }

  if (browser) {
    const saved = localStorage.getItem('opal-docs-theme');
    if (saved === 'light' || saved === 'dark') {
      theme = saved;
    } else if (window.matchMedia('(prefers-color-scheme: light)').matches) {
      theme = 'light';
    }
  }

  $effect(() => {
    if (browser) {
      document.documentElement.dataset.theme = theme;
    }
  });
</script>

<svelte:head>
  <meta name="viewport" content="width=device-width, initial-scale=1" />
</svelte:head>

<div class="shell">
  <aside class="sidebar">
    <div class="sidebar-top">
      <a class="brand" href="/">Opal Docs</a>
      <button class="theme-toggle" onclick={() => applyTheme(theme === 'dark' ? 'light' : 'dark')}>
        {theme === 'dark' ? 'Light' : 'Dark'}
      </button>
    </div>

    <label class="search">
      <input bind:value={query} type="search" placeholder="Search docs" />
    </label>

    <nav>
      {#each filteredDocs as doc}
        <a class:selected={page.url.pathname === hrefFor(doc.slug)} href={hrefFor(doc.slug)}>{doc.title}</a>
      {/each}
      {#if filteredDocs.length === 0}
        <p class="empty">No matches.</p>
      {/if}
    </nav>
  </aside>
  <main class="content">{@render children()}</main>
</div>

<style>
  :global(:root) {
    --bg: #0b1020;
    --bg-soft: #111827;
    --panel: rgba(15, 23, 42, 0.7);
    --panel-strong: #0f172a;
    --border: #24324e;
    --border-strong: #334155;
    --text: #dbe4f0;
    --text-soft: #94a3b8;
    --text-strong: #f8fafc;
    --accent: #38bdf8;
    --accent-soft: rgba(56, 189, 248, 0.12);
    --shadow: 0 24px 48px rgba(0, 0, 0, 0.22);
  }
  :global(html[data-theme='light']) {
    --bg: #f8fafc;
    --bg-soft: #eef4fb;
    --panel: rgba(255, 255, 255, 0.85);
    --panel-strong: #ffffff;
    --border: #d8e2ee;
    --border-strong: #cbd5e1;
    --text: #1e293b;
    --text-soft: #475569;
    --text-strong: #020617;
    --accent: #0284c7;
    --accent-soft: rgba(2, 132, 199, 0.09);
    --shadow: 0 22px 42px rgba(15, 23, 42, 0.08);
  }
  :global(html, body) {
    margin: 0;
    min-height: 100%;
    background: var(--bg);
    color: var(--text);
    font-family: Inter, ui-sans-serif, system-ui, sans-serif;
  }
  :global(body) { min-height: 100vh; }
  .shell {
    min-height: 100vh;
    display: grid;
    grid-template-columns: 280px 1fr;
    background:
      radial-gradient(circle at top left, color-mix(in srgb, var(--accent) 18%, transparent), transparent 35%),
      var(--bg);
  }
  .sidebar {
    position: sticky;
    top: 0;
    align-self: start;
    height: 100vh;
    overflow: auto;
    border-right: 1px solid var(--border);
    background: color-mix(in srgb, var(--panel-strong) 92%, transparent);
    backdrop-filter: blur(18px);
    padding: 1rem;
    box-sizing: border-box;
  }
  .sidebar-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.9rem;
  }
  .brand {
    font-size: 1.2rem;
    font-weight: 800;
    color: var(--text-strong);
    text-decoration: none;
  }
  .theme-toggle {
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--panel);
    color: var(--text-strong);
    padding: 0.45rem 0.75rem;
    cursor: pointer;
  }
  .search input {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
    color: var(--text);
    padding: 0.7rem 0.8rem;
    box-sizing: border-box;
    margin-bottom: 0.9rem;
  }
  nav {
    display: grid;
    gap: 0.15rem;
  }
  nav a {
    color: var(--text-soft);
    text-decoration: none;
    padding: 0.55rem 0.65rem;
    border-radius: 10px;
    transition: 120ms ease;
    font-weight: 500;
  }
  nav a:hover {
    background: var(--accent-soft);
    color: var(--text-strong);
  }
  nav a.selected {
    background: var(--accent-soft);
    color: var(--text-strong);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .empty {
    color: var(--text-soft);
    padding: 0.5rem 0.65rem;
    margin: 0;
  }
  .content {
    min-width: 0;
  }
  @media (max-width: 960px) {
    .shell { grid-template-columns: 1fr; }
    .sidebar {
      position: static;
      height: auto;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
  }
</style>
