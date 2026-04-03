<script lang="ts">
  import { afterNavigate } from '$app/navigation';
  import { browser } from '$app/environment';

  let { data } = $props();

  function scrollToHash() {
    if (!browser) return;
    const hash = window.location.hash;
    if (!hash) return;
    const id = decodeURIComponent(hash.slice(1));
    const target = document.getElementById(id);
    if (target) {
      target.scrollIntoView({ block: 'start' });
    }
  }

  if (browser) {
    afterNavigate(() => {
      requestAnimationFrame(() => scrollToHash());
    });
  }
</script>

<article class="doc" aria-label="Documentation page">
  <header class="doc-header">
    <p class="eyebrow">documentation</p>
    <h1>{data.doc.title}</h1>
    {#if data.doc.summary}
      <p class="summary">{data.doc.summary}</p>
    {/if}
  </header>

  {@html data.doc.html}

  <footer class="pager">
    {#if data.previous}
      <a href={data.previous.slug === 'index' ? '/' : `/docs/${data.previous.slug}`}>← {data.previous.title}</a>
    {:else}
      <span></span>
    {/if}

    {#if data.next}
      <a href={`/docs/${data.next.slug}`}>{data.next.title} →</a>
    {/if}
  </footer>
</article>

<style>
  .doc {
    min-width: 0;
    color: var(--text);
    line-height: 1.7;
  }

  .doc-header {
    margin-bottom: 1.2rem;
    border-bottom: 1px solid var(--pane-border);
    padding-bottom: 0.85rem;
  }

  .eyebrow {
    margin: 0;
    color: var(--muted);
    text-transform: lowercase;
    font-size: 0.86rem;
  }

  h1 {
    margin: 0.2rem 0 0.5rem;
    color: var(--accent);
    font-size: 1.45rem;
    line-height: 1.2;
  }

  .summary {
    margin: 0;
    color: var(--text-dim);
    font-size: 0.95rem;
  }

  .pager {
    display: flex;
    justify-content: space-between;
    gap: 0.8rem;
    margin-top: 1.8rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--pane-border);
  }

  .pager a {
    color: var(--accent);
    text-decoration: none;
    font-size: 0.92rem;
  }

  :global(article h1),
  :global(article h2),
  :global(article h3) {
    color: var(--accent);
    scroll-margin-top: 0.5rem;
  }

  :global(article h2) {
    margin-top: 1.8rem;
  }

  :global(article h3) {
    margin-top: 1.2rem;
  }

  :global(article p),
  :global(article li),
  :global(article td),
  :global(article th) {
    color: var(--text);
  }

  :global(article .heading-anchor) {
    display: inline-block;
    margin-right: 0.45rem;
    color: var(--muted);
    text-decoration: none;
    opacity: 0.5;
  }

  :global(article ul),
  :global(article ol) {
    padding-left: 1.2rem;
  }

  :global(article pre.shiki) {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    padding: 0.7rem 0.8rem !important;
    overflow: auto;
    background: var(--inline-bg) !important;
    line-height: 1.4;
  }

  :global(article .shiki-inline) {
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    padding: 0.05rem 0.3rem;
    background: var(--inline-bg) !important;
  }

  :global(html[data-theme='dark'] article .shiki),
  :global(html[data-theme='dark'] article .shiki span) {
    color: var(--shiki-dark) !important;
    background-color: var(--shiki-dark-bg) !important;
  }

  :global(article .asciinema-embed) {
    margin: 1rem 0;
    border: 1px solid var(--pane-border);
    border-radius: 4px;
    overflow: hidden;
    background: var(--app-bg);
  }

  :global(article .asciinema-embed iframe) {
    display: block;
    width: 100%;
    min-height: 460px;
    border: 0;
    background: #000;
  }

  :global(article table) {
    width: 100%;
    border-collapse: collapse;
    margin: 1rem 0;
  }

  :global(article th),
  :global(article td) {
    border: 1px solid var(--pane-border);
    padding: 0.45rem 0.5rem;
    text-align: left;
  }

  :global(article blockquote) {
    margin: 1rem 0;
    padding: 0.45rem 0.7rem;
    border-left: 3px solid var(--muted);
    background: var(--inline-bg);
  }
</style>
