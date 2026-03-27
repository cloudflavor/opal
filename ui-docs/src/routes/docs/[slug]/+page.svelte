<script lang="ts">
  let { data } = $props();
</script>

<svelte:head>
  <title>{data.doc.title} · Opal Docs</title>
</svelte:head>

<div class="doc-layout">
  <article class="doc card">
    <header>
      <p class="eyebrow">Documentation</p>
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

  {#if data.doc.headings.length > 1}
    <aside class="toc card">
      <h2>On this page</h2>
      <ul>
        {#each data.doc.headings.slice(1) as heading}
          <li class={`depth-${heading.depth}`}><a href={`#${heading.id}`}>{heading.text}</a></li>
        {/each}
      </ul>
    </aside>
  {/if}
</div>

<style>
  .doc-layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 250px;
    gap: 1.5rem;
    padding: 2rem;
  }
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 18px;
    box-shadow: var(--shadow);
    backdrop-filter: blur(16px);
  }
  .doc {
    min-width: 0;
    max-width: 920px;
    padding: 2rem 2.25rem;
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent);
    font-size: 0.75rem;
    font-weight: 700;
  }
  h1 {
    margin-top: 0.35rem;
    color: var(--text-strong);
    font-size: clamp(2rem, 4vw, 3rem);
  }
  .summary {
    color: var(--text-soft);
    font-size: 1.05rem;
    line-height: 1.6;
    margin-bottom: 2rem;
  }
  .toc {
    position: sticky;
    top: 1rem;
    align-self: start;
    padding: 1rem;
  }
  .toc h2 {
    margin-top: 0;
    font-size: 0.9rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-strong);
  }
  .toc ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .toc li + li { margin-top: 0.45rem; }
  .toc li.depth-3 { padding-left: 0.75rem; }
  .toc a {
    color: var(--text-soft);
    text-decoration: none;
  }
  .toc a:hover { color: var(--accent); }
  .pager {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    margin-top: 3rem;
    padding-top: 1rem;
    border-top: 1px solid var(--border);
  }
  .pager a {
    color: var(--accent);
    text-decoration: none;
  }
  :global(article) { line-height: 1.75; }
  :global(article h1), :global(article h2), :global(article h3) {
    color: var(--text-strong);
    scroll-margin-top: 1rem;
  }
  :global(article p), :global(article li) { color: var(--text); }
  :global(article h2) { margin-top: 2.5rem; }
  :global(article h3) { margin-top: 1.75rem; }
  :global(article ul), :global(article ol) { padding-left: 1.35rem; }
  :global(article pre.shiki) {
    padding: 1rem 1.1rem !important;
    overflow: auto;
    border-radius: 14px;
    border: 1px solid var(--border);
    box-shadow: inset 0 1px 0 rgba(255,255,255,0.03);
    line-height: 1.65;
  }
  :global(article pre.shiki code) {
    display: grid;
  }
  :global(article pre.shiki .line) {
    display: block;
    min-height: 1.2rem;
  }
  :global(html[data-theme='light'] article pre.shiki) {
    box-shadow: inset 0 1px 0 rgba(255,255,255,0.5);
  }
  :global(html[data-theme='dark'] article pre.shiki),
  :global(html[data-theme='dark'] article pre.shiki span) {
    background-color: var(--shiki-dark-bg) !important;
    color: var(--shiki-dark) !important;
  }
  :global(article .shiki-inline) {
    display: inline-block;
    padding: 0.08rem 0.38rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    vertical-align: baseline;
  }
  :global(article .shiki-inline code) {
    display: inline;
  }
  :global(html[data-theme='dark'] article .shiki-inline),
  :global(html[data-theme='dark'] article .shiki-inline span) {
    background-color: var(--shiki-dark-bg) !important;
    color: var(--shiki-dark) !important;
  }
  :global(article blockquote) {
    margin: 1.5rem 0;
    padding: 0.9rem 1.1rem;
    border-left: 3px solid var(--accent);
    background: color-mix(in srgb, var(--panel-strong) 82%, transparent);
    color: var(--text-soft);
    border-radius: 0 12px 12px 0;
  }
  :global(article table) {
    width: 100%;
    border-collapse: collapse;
    margin: 1.5rem 0;
    overflow: hidden;
    border-radius: 12px;
  }
  :global(article th), :global(article td) {
    border: 1px solid var(--border);
    padding: 0.65rem 0.75rem;
    text-align: left;
  }
  @media (max-width: 1100px) {
    .doc-layout { grid-template-columns: 1fr; }
    .toc { position: static; }
  }
</style>
