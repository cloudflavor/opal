<script lang="ts">
  import { onMount } from 'svelte';

  let { castId, title } = $props<{ castId: string; title: string }>();
  let host: HTMLDivElement;

  onMount(() => {
    const script = document.createElement('script');
    script.src = `https://asciinema.org/a/${castId}.js`;
    script.id = `asciicast-${castId}`;
    script.async = true;
    host.replaceChildren(script);
  });
</script>

<div class="asciinema-shell" aria-label={title}>
  <div class="asciinema-host" bind:this={host}></div>
</div>

<style>
  .asciinema-shell {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: 16px;
    overflow: hidden;
    background: color-mix(in srgb, var(--panel-strong) 82%, transparent);
    box-shadow: var(--shadow);
  }
  .asciinema-host {
    width: 100%;
    min-height: 420px;
  }
  .asciinema-host :global(iframe) {
    display: block;
    width: 100% !important;
    min-height: 420px;
    border: 0;
  }
</style>
