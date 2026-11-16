<script lang="ts">
  import type { ServerIcon } from "$lib/generated/bindings/ServerIcon";

  // A connection's icon from its server (validated inline PNG/JPEG/WebP data URIs), in
  // the variant for the current color scheme; the name's initial when there is none.
  let { icon, name, size = 16 }: { icon: ServerIcon | null; name: string; size?: number } = $props();

  const initial = $derived(name.trim().charAt(0).toUpperCase() || "?");
  const light = $derived(icon?.light ?? icon?.src ?? null);
  const dark = $derived(icon?.dark ?? icon?.src ?? null);
</script>

<span class="sicon" style:width="{size}px" style:height="{size}px" style:font-size="{Math.round(size * 0.6)}px" aria-hidden="true">
  {#if light && dark}
    <!-- One <img>: the browser loads only the variant for the current scheme. -->
    <picture>
      {#if dark !== light}<source srcset={dark} media="(prefers-color-scheme: dark)" />{/if}
      <img src={light} alt="" draggable="false" />
    </picture>
  {:else}
    {#if light}<img class="light" src={light} alt="" draggable="false" />{:else}<span class="glyph light">{initial}</span>{/if}
    {#if dark}<img class="dark" src={dark} alt="" draggable="false" />{:else}<span class="glyph dark">{initial}</span>{/if}
  {/if}
</span>

<style>
  .sicon {
    display: inline-flex;
    flex: none;
    position: relative;
  }
  picture,
  img,
  .glyph {
    width: 100%;
    height: 100%;
  }
  picture {
    display: block;
  }
  img {
    display: block;
    object-fit: contain;
  }
  .glyph {
    display: inline-grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-weight: 600;
    line-height: 1;
  }
  .dark {
    display: none;
  }
  @media (prefers-color-scheme: dark) {
    .light {
      display: none;
    }
    img.dark {
      display: block;
    }
    .glyph.dark {
      display: inline-grid;
    }
  }
</style>
