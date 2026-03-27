<script lang="ts">
  let { data } = $props();
  const featured = $derived(data.docs.filter((doc) => doc.slug !== 'index'));
</script>

<svelte:head>
  <title>Opal Docs</title>
</svelte:head>

<section class="hero">
  <p class="eyebrow">Documentation</p>
  <h1>Run GitLab-style pipelines locally with confidence.</h1>
  <p class="lede">Browse the supported local subset, parity notes, runtime configuration, and release checklist.</p>
</section>

<section class="cards">
  {#each featured as doc}
    <a class="card" href={`/docs/${doc.slug}`}>
      <h2>{doc.title}</h2>
      <p>{doc.summary}</p>
    </a>
  {/each}
</section>

<style>
  .hero {
    padding: 3rem 3rem 1rem;
    max-width: 900px;
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent);
    font-size: 0.75rem;
    font-weight: 700;
  }
  h1 {
    margin: 0.35rem 0 0.85rem;
    font-size: clamp(2rem, 5vw, 3.5rem);
    line-height: 1.05;
    color: var(--text-strong);
  }
  .lede {
    max-width: 60ch;
    color: var(--text-soft);
    font-size: 1.05rem;
    line-height: 1.6;
  }
  .cards {
    padding: 1rem 3rem 3rem;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 1rem;
  }
  .card {
    display: block;
    padding: 1rem 1.1rem;
    border: 1px solid var(--border);
    border-radius: 14px;
    background: var(--panel-soft);
    text-decoration: none;
    color: inherit;
  }
  .card:hover {
    border-color: var(--accent);
    transform: translateY(-1px);
  }
  .card h2 {
    margin: 0 0 0.5rem;
    font-size: 1.1rem;
    color: var(--text-strong);
  }
  .card p {
    margin: 0;
    color: var(--text-soft);
    line-height: 1.45;
  }
</style>
