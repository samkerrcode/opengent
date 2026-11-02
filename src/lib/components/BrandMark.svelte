<script lang="ts">
  import mark from "../../../docs/assets/logo.svg?no-inline";

  let { size = 24 }: { size?: number } = $props();
</script>

<span
  class="brand-mark"
  style:width="{size}px"
  style:height="{size}px"
  style:--brand-mark={`url("${mark}")`}
  aria-hidden="true"
></span>

<style>
  .brand-mark {
    display: inline-block;
    flex: none;
    background: var(--accent);
    -webkit-mask: var(--brand-mark) center / contain no-repeat;
    mask: var(--brand-mark) center / contain no-repeat;
  }
</style>
