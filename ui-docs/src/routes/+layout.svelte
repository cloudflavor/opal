<script lang="ts">
  import { afterNavigate, goto } from '$app/navigation';
  import { browser } from '$app/environment';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import type { DocPage } from '$lib/types';

  let { data, children } = $props();
  let query = $state('');
  let theme = $state<'dark' | 'light'>('light');
  let showMenu = $state(false);
  let showAnchors = $state(true);
  let showCloudflavorDialog = $state(false);
  let searchInput: HTMLInputElement | null = null;
  let contentPane: HTMLElement | null = null;
  let pageAnchors = $state<{ id: string; text: string; depth: number }[]>([]);
  let pendingAnchorRefresh = false;

  const docs = $derived((data.docs ?? []) as DocPage[]);
  const SITE_URL = 'https://opal.cloudflavor.io';
  const SITE_NAME = 'Opal Docs';
  const DEFAULT_DESCRIPTION =
    'Run and debug GitLab-style CI pipelines locally with Opal. Learn install, quickstart, pipeline planning, and UI workflows.';
  type SearchResult = {
    key: string;
    slug: string;
    anchorId?: string;
    title: string;
    label: string;
    snippet?: string;
  };

  function normalizeAnchorText(text: string): string {
    return text
      .replace(/^#+\s*/, '')
      .replace(/`([^`]+)`/g, '$1')
      .replace(/\s+/g, ' ')
      .trim();
  }

  function htmlToText(html: string): string {
    return normalizeAnchorText(html.replace(/<[^>]+>/g, ' '));
  }

  function makeSnippet(haystack: string, needle: string): string | undefined {
    const lower = haystack.toLowerCase();
    const idx = lower.indexOf(needle);
    if (idx < 0) return undefined;
    const start = Math.max(0, idx - 54);
    const end = Math.min(haystack.length, idx + needle.length + 82);
    return `${start > 0 ? '…' : ''}${haystack.slice(start, end)}${end < haystack.length ? '…' : ''}`;
  }

  const searchResults = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return [] as SearchResult[];

    const seen = new Set<string>();
    const out: SearchResult[] = [];

    for (const doc of docs) {
      const titleMatch = doc.title.toLowerCase().includes(needle);
      const summaryMatch = doc.summary.toLowerCase().includes(needle);
      const slugMatch = doc.slug.toLowerCase().includes(needle);
      if (titleMatch || summaryMatch || slugMatch) {
        const key = `doc:${doc.slug}`;
        if (!seen.has(key)) {
          seen.add(key);
          out.push({
            key,
            slug: doc.slug,
            title: doc.title,
            label: doc.title,
            snippet: doc.summary || `open ${doc.slug}`
          });
        }
      }

      for (const heading of doc.headings.slice(1)) {
        const text = normalizeAnchorText(heading.text);
        if (!text || !text.toLowerCase().includes(needle)) continue;
        const key = `heading:${doc.slug}:${heading.id}`;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push({
          key,
          slug: doc.slug,
          anchorId: heading.id,
          title: doc.title,
          label: text,
          snippet: doc.title
        });
      }

      const bodyText = htmlToText(doc.html);
      const snippet = makeSnippet(bodyText, needle);
      if (!snippet) continue;
      const key = `body:${doc.slug}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({
        key,
        slug: doc.slug,
        title: doc.title,
        label: `${doc.title} (content)`,
        snippet
      });
    }

    return out.slice(0, 24);
  });

  const currentSlug = $derived.by(() => {
    const path = page.url.pathname;
    if (path === '/') return 'index';
    if (path.startsWith('/docs/')) return decodeURIComponent(path.slice('/docs/'.length));
    return 'index';
  });

  const currentDocIndex = $derived.by(() => docs.findIndex((doc) => doc.slug === currentSlug));
  const currentDoc = $derived.by(() => docs[currentDocIndex] ?? docs[0] ?? null);
  const isHomePage = $derived.by(() => page.url.pathname === '/');
  const metaTitle = $derived.by(() => {
    if (isHomePage) return SITE_NAME;
    return currentDoc ? `${currentDoc.title} · ${SITE_NAME}` : SITE_NAME;
  });
  const metaDescription = $derived.by(() => {
    const description = currentDoc?.summary?.trim();
    return description || DEFAULT_DESCRIPTION;
  });
  const canonicalUrl = $derived.by(() => {
    const path = page.url.pathname || '/';
    return new URL(path, SITE_URL).toString();
  });
  const ogType = $derived.by(() => (isHomePage ? 'website' : 'article'));
  const ogImageUrl = new URL('/favicon.svg', SITE_URL).toString();
  const jsonLd = $derived.by(() => {
    if (isHomePage) {
      return JSON.stringify(
        {
          '@context': 'https://schema.org',
          '@type': 'WebSite',
          name: SITE_NAME,
          url: SITE_URL,
          publisher: {
            '@type': 'Organization',
            name: 'Cloudflavor',
            url: 'https://cloudflavor.io'
          }
        },
        null,
        2
      );
    }

    return JSON.stringify(
      {
        '@context': 'https://schema.org',
        '@type': 'TechArticle',
        headline: currentDoc?.title ?? SITE_NAME,
        description: metaDescription,
        url: canonicalUrl,
        publisher: {
          '@type': 'Organization',
          name: 'Cloudflavor',
          url: 'https://cloudflavor.io'
        }
      },
      null,
      2
    );
  });
  const fallbackAnchors = $derived.by(() =>
    (currentDoc?.headings.slice(1) ?? [])
      .filter((heading) => heading.depth === 2)
      .map((heading) => ({
        id: heading.id,
        text: normalizeAnchorText(heading.text),
        depth: heading.depth
      }))
      .filter((heading) => heading.text.length > 0)
  );
  const activeAnchors = $derived.by(() => (pageAnchors.length ? pageAnchors : fallbackAnchors));

  function slugFromPath(pathname: string): string {
    if (pathname === '/') return 'index';
    if (pathname.startsWith('/docs/')) return decodeURIComponent(pathname.slice('/docs/'.length));
    return 'index';
  }

  function hrefFor(slug: string) {
    return slug === 'index' ? '/' : `/docs/${slug}`;
  }

  function activeIndexIn(list: DocPage[]) {
    const index = list.findIndex((doc) => doc.slug === currentSlug);
    return index >= 0 ? index : 0;
  }

  function navigateByAbsoluteIndex(index: number) {
    if (index < 0 || index >= docs.length) return;
    goto(hrefFor(docs[index].slug));
  }

  function navigateByVisibleDelta(delta: number) {
    const list = docs;
    if (!list.length) return;
    const start = activeIndexIn(list);
    const next = (start + delta + list.length) % list.length;
    goto(hrefFor(list[next].slug));
  }

  function applyTheme(nextTheme: 'dark' | 'light') {
    theme = nextTheme;
    if (browser) {
      document.documentElement.dataset.theme = nextTheme;
      localStorage.setItem('opal-docs-theme', nextTheme);
    }
  }

  function focusSearch() {
    searchInput?.focus();
    searchInput?.select();
  }

  function selectSearchResult(result: SearchResult) {
    const hash = result.anchorId ? `#${encodeURIComponent(result.anchorId)}` : '';
    goto(`${hrefFor(result.slug)}${hash}`);
    query = '';
  }

  function onSearchKeyDown(event: KeyboardEvent) {
    if (event.key === 'Enter' && searchResults.length) {
      event.preventDefault();
      selectSearchResult(searchResults[0]);
      return;
    }

    if (event.key === 'Escape') {
      query = '';
      return;
    }
  }

  function scrollStep(): number {
    if (!contentPane) return 240;
    return Math.max(36, Math.round(contentPane.clientHeight * 0.08));
  }

  function scrollPageStep(): number {
    if (!contentPane) return 640;
    return Math.max(120, Math.round(contentPane.clientHeight * 0.9));
  }

  function scrollHalfStep(): number {
    if (!contentPane) return 360;
    return Math.max(80, Math.round(contentPane.clientHeight * 0.45));
  }

  function scrollContentBy(delta: number) {
    contentPane?.scrollBy({ top: delta, behavior: 'auto' });
  }

  function scrollContentToTop() {
    contentPane?.scrollTo({ top: 0, behavior: 'auto' });
  }

  function scrollContentToBottom() {
    if (!contentPane) return;
    contentPane.scrollTo({ top: contentPane.scrollHeight, behavior: 'auto' });
  }

  function collectAnchors() {
    if (!contentPane) return;
    const nodes = contentPane.querySelectorAll<HTMLElement>('h2[id]');
    const anchors = Array.from(nodes)
      .map((node) => {
        const clone = node.cloneNode(true) as HTMLElement;
        clone.querySelector('.heading-anchor')?.remove();
        const text = normalizeAnchorText(clone.textContent ?? '');
        if (!node.id || !text) return null;
        const depth = Number(node.tagName.slice(1));
        return { id: node.id, text, depth: Number.isFinite(depth) ? depth : 2 };
      })
      .filter((entry): entry is { id: string; text: string; depth: number } => entry !== null);
    pageAnchors = anchors;
  }

  function refreshAnchorsSoon() {
    if (!browser || pendingAnchorRefresh) return;
    pendingAnchorRefresh = true;
    requestAnimationFrame(() => {
      pendingAnchorRefresh = false;
      collectAnchors();
    });
  }

  function jumpToAnchor(id: string) {
    const target = contentPane?.querySelector<HTMLElement>(`#${CSS.escape(id)}`);
    if (!target) return;
    target.scrollIntoView({ block: 'start', behavior: 'auto' });
    if (browser) {
      const next = `${window.location.pathname}${window.location.search}#${encodeURIComponent(id)}`;
      window.history.replaceState(null, '', next);
    }
  }

  function onGlobalKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    const tag = target?.tagName;
    const isInputTarget =
      target?.isContentEditable ||
      tag === 'INPUT' ||
      tag === 'TEXTAREA' ||
      tag === 'SELECT';

    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === '?') {
      if (!isInputTarget) {
        event.preventDefault();
        showCloudflavorDialog = !showCloudflavorDialog;
      }
      return;
    }

    if (showCloudflavorDialog && event.key === 'Escape') {
      event.preventDefault();
      showCloudflavorDialog = false;
      return;
    }

    if (showCloudflavorDialog) return;

    if (event.key === 'H') {
      event.preventDefault();
      showMenu = !showMenu;
      return;
    }

    if (event.key === 'Y') {
      event.preventDefault();
      showAnchors = !showAnchors;
      return;
    }

    if (event.key === '/' && !isInputTarget) {
      event.preventDefault();
      focusSearch();
      return;
    }

    if (isInputTarget || !docs.length) return;

    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') {
      event.preventDefault();
      navigateByVisibleDelta(1);
      return;
    }

    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') {
      event.preventDefault();
      navigateByVisibleDelta(-1);
      return;
    }

    if (event.key === 'j') {
      event.preventDefault();
      scrollContentBy(scrollStep());
      return;
    }

    if (event.key === 'k') {
      event.preventDefault();
      scrollContentBy(-scrollStep());
      return;
    }

    if (event.key === ' ' || event.key === 'PageDown') {
      event.preventDefault();
      scrollContentBy(scrollPageStep());
      return;
    }

    if (event.key === 'b' || event.key === 'PageUp') {
      event.preventDefault();
      scrollContentBy(-scrollPageStep());
      return;
    }

    if (event.key === 'd') {
      event.preventDefault();
      scrollContentBy(scrollHalfStep());
      return;
    }

    if (event.key === 'u') {
      event.preventDefault();
      scrollContentBy(-scrollHalfStep());
      return;
    }

    if (event.key === 'g') {
      event.preventDefault();
      scrollContentToTop();
      return;
    }

    if (event.key === 'G') {
      event.preventDefault();
      scrollContentToBottom();
      return;
    }
  }

  onMount(() => {
    if (!browser) return;

    const saved = localStorage.getItem('opal-docs-theme');
    if (saved === 'dark' || saved === 'light') {
      theme = saved;
    } else if (window.matchMedia('(prefers-color-scheme: dark)').matches) {
      theme = 'dark';
    }
    document.documentElement.dataset.theme = theme;
    refreshAnchorsSoon();
    afterNavigate((navigation) => {
      const toPath = navigation.to?.url.pathname ?? '';
      const fromPath = navigation.from?.url.pathname ?? '';
      const toSlug = slugFromPath(toPath);
      const fromSlug = slugFromPath(fromPath);
      if (toSlug !== fromSlug) {
        contentPane?.scrollTo({ top: 0, left: 0, behavior: 'auto' });
      }
      refreshAnchorsSoon();
    });

    const observer = new MutationObserver(() => refreshAnchorsSoon());
    if (contentPane) {
      observer.observe(contentPane, {
        childList: true,
        subtree: true
      });
    }

    window.addEventListener('keydown', onGlobalKey);
    return () => {
      observer.disconnect();
      window.removeEventListener('keydown', onGlobalKey);
    };
  });
</script>

<svelte:head>
  <title>{metaTitle}</title>
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <meta name="description" content={metaDescription} />
  <meta name="robots" content="index, follow, max-snippet:-1, max-image-preview:large, max-video-preview:-1" />
  <link rel="canonical" href={canonicalUrl} />

  <meta property="og:site_name" content={SITE_NAME} />
  <meta property="og:type" content={ogType} />
  <meta property="og:title" content={metaTitle} />
  <meta property="og:description" content={metaDescription} />
  <meta property="og:url" content={canonicalUrl} />
  <meta property="og:image" content={ogImageUrl} />

  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content={metaTitle} />
  <meta name="twitter:description" content={metaDescription} />
  <meta name="twitter:image" content={ogImageUrl} />

  <script type="application/ld+json">{jsonLd}</script>
</svelte:head>

<div
  class="tui-shell"
  class:menu-open={showMenu}
  class:anchors-open={showAnchors}
  aria-label="Opal docs interface"
>
  {#if showMenu}
    <aside class="pane menu-pane" aria-label="History sidebar">
      <div class="menu-head">
        <p class="pane-title">history</p>
        <p class="pane-title">H to close</p>
      </div>
      <nav class="menu-list" aria-label="Documents">
        {#each docs as doc}
          <a class:active={doc.slug === currentSlug} href={hrefFor(doc.slug)}>
            {doc.title}
          </a>
        {/each}
        {#if !docs.length}
          <p class="empty">no matches</p>
        {/if}
      </nav>
    </aside>
  {/if}

  <section class="center-stack">
    <header class="pane top-tabs" aria-label="Top menu tabs">
      <div class="top-row">
        <div class="top-left">
          <p class="pane-title">jobs {docs.length ? activeIndexIn(docs) + 1 : 0}/{docs.length || 0}</p>
          <button class="menu-toggle" type="button" onclick={() => (showMenu = !showMenu)}>
            H menu
          </button>
        </div>

        <label class="search">
          <input
            bind:this={searchInput}
            bind:value={query}
            type="search"
            placeholder="search website content"
            aria-label="Search docs"
            onkeydown={onSearchKeyDown}
          />
        </label>

        <div class="top-right">
          <button
            class="theme-toggle"
            type="button"
            onclick={() => applyTheme(theme === 'dark' ? 'light' : 'dark')}
          >
            {theme === 'dark' ? 'light' : 'dark'}
          </button>
        </div>
      </div>

      {#if query.trim()}
        <div class="search-results" role="listbox" aria-label="Search results">
          <p class="pane-title">search results {searchResults.length}</p>
          {#if searchResults.length}
            <ul>
              {#each searchResults as result}
                <li>
                  <button type="button" onclick={() => selectSearchResult(result)}>
                    <span class="result-label">{result.label}</span>
                    <span class="result-meta">{result.slug}{result.anchorId ? `#${result.anchorId}` : ''}</span>
                    {#if result.snippet}
                      <span class="result-snippet">{result.snippet}</span>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="empty">no matches in docs content</p>
          {/if}
        </div>
      {/if}

      <div class="tab-strip" role="tablist" aria-label="Docs tabs">
        {#each docs as doc, index}
          <button
            type="button"
            role="tab"
            aria-selected={doc.slug === currentSlug}
            class="tab"
            class:active={doc.slug === currentSlug}
            onclick={() => goto(hrefFor(doc.slug))}
          >
            <span class="tab-marker">{doc.slug === currentSlug ? '›' : '·'}</span>
            <span class="tab-name">{doc.slug}</span>
          </button>
        {/each}
        {#if !docs.length}
          <p class="empty">no matches</p>
        {/if}
      </div>
    </header>

    <main class="pane content-pane" aria-label="Content" bind:this={contentPane}>
      {@render children()}
    </main>

    <footer class="pane shortcuts" aria-label="Keyboard shortcuts">
      <p class="pane-title">shortcuts</p>
      <div class="shortcut-row">
        <button type="button" class="shortcut" onclick={() => navigateByVisibleDelta(-1)}>
          <span class="key">← / ↑</span>
          <span class="label">prev tab</span>
        </button>
        <button type="button" class="shortcut" onclick={() => navigateByVisibleDelta(1)}>
          <span class="key">→ / ↓</span>
          <span class="label">next tab</span>
        </button>
        <button type="button" class="shortcut" onclick={focusSearch}>
          <span class="key">/</span>
          <span class="label">search</span>
        </button>
        <button type="button" class="shortcut" onclick={() => (showMenu = !showMenu)}>
          <span class="key">H</span>
          <span class="label">menu</span>
        </button>
        <button type="button" class="shortcut" onclick={() => (showAnchors = !showAnchors)}>
          <span class="key">Y</span>
          <span class="label">anchors</span>
        </button>
        <button type="button" class="shortcut" onclick={() => (showCloudflavorDialog = !showCloudflavorDialog)}>
          <span class="key">?</span>
          <span class="label">cloudflavor</span>
        </button>
        <button type="button" class="shortcut" onclick={() => scrollContentBy(scrollStep())}>
          <span class="key">j/k</span>
          <span class="label">line scroll</span>
        </button>
        <button type="button" class="shortcut" onclick={() => scrollContentBy(scrollPageStep())}>
          <span class="key">space / b</span>
          <span class="label">page scroll</span>
        </button>
      </div>
    </footer>
  </section>

  {#if showAnchors}
    <aside class="pane right-pane" aria-label="On this page">
      <div class="right-head">
        <p class="pane-title">anchors</p>
        <p class="right-context">{currentDoc?.slug ?? 'index'}</p>
        <p class="right-hint">Y to close</p>
      </div>

      <div class="right-body">
        {#if activeAnchors.length}
          <ul class="heading-list">
            {#each activeAnchors as anchor}
              <li class={`depth-${anchor.depth}`}>
                <button type="button" onclick={() => jumpToAnchor(anchor.id)}>
                  {anchor.text}
                </button>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="empty">no section anchors</p>
        {/if}
      </div>
    </aside>
  {/if}
</div>

{#if showCloudflavorDialog}
  <div
    class="brand-modal-backdrop"
    role="presentation"
    onclick={(event) => {
      if (event.target === event.currentTarget) showCloudflavorDialog = false;
    }}
  >
    <div class="pane brand-modal" role="dialog" aria-modal="true" aria-label="Produced by Cloudflavor">
      <header class="brand-head">
        <div class="brand-title-wrap">
          <svg class="brand-logo" viewBox="0 0 32 32" aria-hidden="true">
            <path d="M9 20a5 5 0 1 1 1.1-9.9A7 7 0 0 1 23.4 11a4.5 4.5 0 1 1 .6 9H9z" />
          </svg>
          <div>
            <p class="pane-title">about</p>
            <h2 class="brand-title">Produced by Cloudflavor</h2>
          </div>
        </div>
        <button class="brand-close" type="button" onclick={() => (showCloudflavorDialog = false)}>close</button>
      </header>

      <pre class="brand-terminal"><span class="prompt">$</span> opal docs --about
Powered by Cloudflavor.
Visit: https://cloudflavor.io</pre>

      <a class="brand-link" href="https://cloudflavor.io" target="_blank" rel="noreferrer">
        open cloudflavor.io
      </a>
      <p class="brand-hint">Press <code>?</code> or <code>Esc</code> to close.</p>
    </div>
  </div>
{/if}

<style>
  :global(:root) {
    --app-bg: #eff1f5;
    --pane-bg: #e6e9ef;
    --pane-border: #bcc0cc;
    --text: #4c4f69;
    --text-dim: #5c5f77;
    --muted: #6c6f85;
    --accent: #1e66f5;
    --inline-bg: #dce0e8;
    --tab-bg: #e6e9ef;
    --tab-active-bg: #dce0e8;
    --key: #fe640b;
    --button-text: #5c5f77;
    --scroll-track: color-mix(in srgb, var(--pane-bg) 70%, transparent);
    --scroll-thumb: color-mix(in srgb, var(--pane-border) 72%, var(--pane-bg));
    --scroll-thumb-hover: color-mix(in srgb, var(--accent) 32%, var(--pane-border));
  }

  :global(html[data-theme='dark']) {
    --app-bg: #1e1e2e;
    --pane-bg: #181825;
    --pane-border: #45475a;
    --text: #cdd6f4;
    --text-dim: #bac2de;
    --muted: #a6adc8;
    --accent: #89b4fa;
    --inline-bg: #313244;
    --tab-bg: #181825;
    --tab-active-bg: #313244;
    --key: #fab387;
    --button-text: #bac2de;
    --scroll-track: color-mix(in srgb, var(--pane-bg) 64%, transparent);
    --scroll-thumb: color-mix(in srgb, var(--pane-border) 62%, var(--pane-bg));
    --scroll-thumb-hover: color-mix(in srgb, var(--accent) 40%, var(--pane-border));
  }

  :global(html, body) {
    margin: 0;
    height: 100%;
    background: var(--app-bg);
    color: var(--text);
    font-family: ui-monospace, 'SFMono-Regular', Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
    overflow: hidden;
  }

  :global(body) {
    min-height: 100%;
  }

  :global(*) {
    scrollbar-width: thin;
    scrollbar-color: var(--scroll-thumb) var(--scroll-track);
  }

  :global(*::-webkit-scrollbar) {
    width: 9px;
    height: 9px;
  }

  :global(*::-webkit-scrollbar-track) {
    background: var(--scroll-track);
    border-radius: 999px;
  }

  :global(*::-webkit-scrollbar-thumb) {
    background: var(--scroll-thumb);
    border-radius: 999px;
    border: 2px solid transparent;
    background-clip: padding-box;
  }

  :global(*::-webkit-scrollbar-thumb:hover) {
    background: var(--scroll-thumb-hover);
    border: 2px solid transparent;
    background-clip: padding-box;
  }

  .tui-shell {
    height: 100vh;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 1px;
    padding: 1px;
    box-sizing: border-box;
    background: var(--pane-border);
    overflow: hidden;
  }

  .tui-shell.menu-open {
    grid-template-columns: clamp(240px, 22vw, 340px) minmax(0, 1fr);
  }

  .tui-shell.anchors-open {
    grid-template-columns: minmax(0, 1fr) clamp(250px, 27vw, 360px);
  }

  .tui-shell.menu-open.anchors-open {
    grid-template-columns: clamp(240px, 22vw, 340px) minmax(0, 1fr) clamp(250px, 27vw, 360px);
  }

  .menu-pane {
    grid-column: 1;
    grid-row: 1;
    height: calc(100vh - 2px);
    display: grid;
    grid-template-rows: auto 1fr;
    gap: 0.4rem;
    overflow: hidden;
  }

  .menu-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }

  .menu-toggle {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    background: var(--tab-bg);
    color: var(--button-text);
    padding: 0.14rem 0.42rem;
    cursor: pointer;
    font: inherit;
  }

  .menu-list {
    overflow: auto;
    display: grid;
    gap: 0.25rem;
    align-content: start;
    padding-right: 0.1rem;
  }

  .menu-list a {
    border: 1px solid transparent;
    border-radius: 4px;
    color: var(--text-dim);
    text-decoration: none;
    padding: 0.18rem 0.38rem;
    font-size: 0.9rem;
  }

  .menu-list a:hover {
    border-color: var(--pane-border);
    background: var(--tab-active-bg);
  }

  .menu-list a.active {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--pane-border));
    background: var(--tab-active-bg);
    font-weight: 700;
  }

  .pane {
    border: 0;
    border-radius: 0;
    background: var(--pane-bg);
    color: var(--text);
    padding: 6px;
    box-sizing: border-box;
  }

  .pane-title {
    margin: 0;
    color: var(--muted);
    font-size: 0.86rem;
  }

  .center-stack {
    grid-column: 1;
    grid-row: 1;
    min-width: 0;
    height: calc(100vh - 2px);
    display: grid;
    grid-template-rows: auto 1fr auto;
    gap: 1px;
    background: var(--pane-border);
    overflow: hidden;
  }

  .tui-shell.menu-open .center-stack {
    grid-column: 2;
  }

  .top-tabs {
    display: grid;
    gap: 0.45rem;
  }

  .top-row {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    gap: 0.6rem;
  }

  .top-left {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    justify-self: start;
  }

  .top-right {
    justify-self: end;
  }

  .search {
    justify-self: center;
  }

  .search input {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    background: var(--tab-bg);
    color: var(--text);
    padding: 0.18rem 0.42rem;
    font: inherit;
    min-width: 13rem;
    width: min(44vw, 36rem);
  }

  .search-results {
    border: 1px solid var(--pane-border);
    background: var(--tab-bg);
    padding: 0.35rem 0.45rem 0.42rem;
    max-height: 15rem;
    overflow: auto;
    display: grid;
    gap: 0.32rem;
  }

  .search-results ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.18rem;
  }

  .search-results button {
    width: 100%;
    text-align: left;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text);
    font: inherit;
    padding: 0.2rem 0.28rem;
    cursor: pointer;
    display: grid;
    gap: 0.08rem;
  }

  .search-results button:hover {
    border-color: var(--pane-border);
    background: var(--tab-active-bg);
  }

  .result-label {
    color: var(--text);
    font-weight: 700;
  }

  .result-meta {
    color: var(--muted);
    font-size: 0.8rem;
  }

  .result-snippet {
    color: var(--text-dim);
    font-size: 0.82rem;
    line-height: 1.35;
  }

  .theme-toggle {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    background: var(--tab-bg);
    color: var(--button-text);
    padding: 0.18rem 0.45rem;
    cursor: pointer;
    font: inherit;
    text-transform: lowercase;
  }

  .tab-strip {
    display: flex;
    flex-wrap: wrap;
    gap: 0;
    overflow: visible;
    white-space: normal;
    align-items: center;
    row-gap: 0.2rem;
  }

  .tab {
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--text-dim);
    padding: 0 0.32rem;
    cursor: pointer;
    font: inherit;
    display: inline-flex;
    align-items: center;
    gap: 0.22rem;
    white-space: normal;
  }

  .tab + .tab::before {
    content: '|';
    color: var(--muted);
    margin-right: 0.35rem;
  }

  .tab-marker {
    color: var(--muted);
    font-weight: 700;
  }

  .tab.active {
    color: var(--text);
    font-weight: 700;
  }

  .tab.active .tab-marker {
    color: var(--accent);
  }

  .content-pane {
    overflow: auto;
    min-height: 0;
  }

  .shortcuts {
    display: grid;
    gap: 0.35rem;
  }

  .shortcut-row {
    display: flex;
    gap: 0.35rem;
    flex-wrap: wrap;
  }

  .shortcut {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    background: var(--tab-bg);
    color: var(--button-text);
    padding: 0.16rem 0.42rem;
    cursor: pointer;
    font: inherit;
    display: inline-flex;
    align-items: center;
    gap: 0.24rem;
  }

  .shortcut .key {
    color: var(--key);
    font-weight: 700;
  }

  .shortcut .label {
    color: var(--text-dim);
    font-weight: 700;
  }

  .right-pane {
    grid-row: 1;
    position: sticky;
    top: 1px;
    height: calc(100vh - 2px);
    align-self: start;
    display: grid;
    grid-template-rows: auto 1fr;
    padding: 0;
    overflow: hidden;
  }

  .tui-shell.anchors-open:not(.menu-open) .right-pane {
    grid-column: 2;
  }

  .tui-shell.menu-open.anchors-open .right-pane {
    grid-column: 3;
  }

  .right-head {
    display: grid;
    gap: 0.18rem;
    padding: 0.4rem 0.5rem 0.45rem;
    border-bottom: 1px solid var(--pane-border);
    background: color-mix(in srgb, var(--pane-bg) 88%, var(--tab-bg));
  }

  .right-context {
    margin: 0;
    color: var(--text-dim);
    font-size: 0.82rem;
    line-height: 1.2;
    text-overflow: ellipsis;
    overflow: hidden;
    white-space: nowrap;
  }

  .right-hint {
    margin: 0;
    color: var(--muted);
    font-size: 0.82rem;
    line-height: 1.2;
  }

  .right-body {
    overflow: auto;
    padding: 0.38rem 0.48rem 0.55rem;
  }

  .heading-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0;
  }

  .heading-list li {
    padding: 0.24rem 0 0.26rem;
    border-top: 1px solid color-mix(in srgb, var(--pane-border) 48%, transparent);
  }

  .heading-list li:first-child {
    border-top: 0;
  }

  .heading-list li.depth-3 {
    padding-left: 0.7rem;
  }

  .heading-list button {
    border: 0;
    background: transparent;
    padding: 0.02rem 0.08rem;
    color: var(--text-dim);
    cursor: pointer;
    font: inherit;
    text-align: left;
    display: block;
    width: 100%;
    line-height: 1.35;
  }

  .heading-list button:hover {
    color: var(--accent);
  }

  .empty {
    margin: 0.2rem 0 0;
    color: var(--muted);
    font-size: 0.9rem;
  }

  .brand-modal-backdrop {
    position: fixed;
    inset: 0;
    z-index: 70;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--app-bg) 72%, transparent);
    backdrop-filter: blur(1px);
    padding: 1rem;
  }

  .brand-modal {
    width: min(92vw, 700px);
    border: 1px solid var(--pane-border);
    background: var(--pane-bg);
    padding: 0.65rem;
    display: grid;
    gap: 0.62rem;
    box-shadow: 0 14px 48px color-mix(in srgb, var(--app-bg) 78%, transparent);
  }

  .brand-head {
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: 0.8rem;
  }

  .brand-title-wrap {
    display: inline-flex;
    align-items: center;
    gap: 0.62rem;
  }

  .brand-logo {
    width: 1.45rem;
    height: 1.45rem;
    fill: var(--accent);
    flex: 0 0 auto;
  }

  .brand-title {
    margin: 0.1rem 0 0;
    font-size: 1rem;
    line-height: 1.2;
    color: var(--text);
  }

  .brand-close {
    border: 1px solid var(--pane-border);
    background: var(--tab-bg);
    color: var(--button-text);
    padding: 0.14rem 0.44rem;
    font: inherit;
    cursor: pointer;
  }

  .brand-terminal {
    margin: 0;
    border: 1px solid var(--pane-border);
    background: var(--app-bg);
    color: var(--text);
    padding: 0.64rem 0.68rem;
    line-height: 1.4;
    white-space: pre-wrap;
  }

  .brand-terminal .prompt {
    color: var(--key);
    font-weight: 700;
  }

  .brand-link {
    color: var(--accent);
    text-decoration: none;
    width: fit-content;
    font-weight: 700;
  }

  .brand-link:hover {
    text-decoration: underline;
  }

  .brand-hint {
    margin: 0;
    color: var(--muted);
    font-size: 0.84rem;
  }

  @media (max-width: 1280px) {
    .search input {
      min-width: 10rem;
      width: min(42vw, 28rem);
    }
  }

  @media (max-width: 980px) {
    :global(html, body) {
      overflow: auto;
    }

    .tui-shell,
    .tui-shell.menu-open,
    .tui-shell.anchors-open,
    .tui-shell.menu-open.anchors-open {
      height: auto;
      grid-template-columns: 1fr;
      overflow: visible;
    }

    .center-stack {
      grid-column: 1;
      height: auto;
      grid-template-rows: auto minmax(380px, 1fr) auto;
      overflow: visible;
    }

    .menu-pane,
    .right-pane {
      grid-column: 1;
      grid-row: auto;
      position: static;
      height: auto;
      overflow: visible;
    }

    .content-pane {
      min-height: 0;
    }

    .top-row {
      grid-template-columns: 1fr;
      justify-items: start;
    }

    .search,
    .top-right {
      justify-self: start;
    }

    .search input {
      width: min(92vw, 26rem);
    }

    .right-pane {
      position: static;
      height: auto;
      max-height: 50vh;
    }
  }
</style>
