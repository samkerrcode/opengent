<script lang="ts">
  let {
    busy,
    onsend,
    onstop,
  }: { busy: boolean; onsend: (text: string) => void; onstop: () => void } = $props();

  let value = $state("");
  let textarea = $state<HTMLTextAreaElement | null>(null);

  function submit() {
    const text = value.trim();
    if (!text || busy) return;
    onsend(text);
    value = "";
    textarea?.focus();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      submit();
    }
    if (e.key === "Escape" && busy) onstop();
  }

  // Grow with content up to a cap; CSS handles the max via max-height.
  $effect(() => {
    void value;
    if (textarea) {
      textarea.style.height = "auto";
      textarea.style.height = `${Math.min(textarea.scrollHeight, 240)}px`;
    }
  });
</script>

<form class="composer" onsubmit={(e) => (e.preventDefault(), submit())}>
  <div class="box">
    <label class="visually-hidden" for="composer-input">Message</label>
    <textarea
      id="composer-input"
      bind:this={textarea}
      bind:value
      rows="1"
      placeholder="Message… (Enter to send, Shift+Enter for a new line)"
      onkeydown={onKeydown}
      autocomplete="off"
      spellcheck="true"
    ></textarea>
    <div class="row">
      <div class="context">
        <span class="chip" title="Provider selection arrives in Phase 2">Simulated provider</span>
        <span class="chip muted" title="Attachments arrive in Phase 1">No attachments</span>
        <span class="chip muted" title="MCP servers arrive in Phase 3">No tools</span>
      </div>
      {#if busy}
        <button type="button" class="btn-outline" onclick={onstop} title="Stop (Esc)">Stop</button>
      {:else}
        <button type="submit" class="btn-primary" disabled={!value.trim()}>Send</button>
      {/if}
    </div>
  </div>
</form>

<style>
  .composer {
    padding: 0 var(--space-6) var(--space-6);
  }
  .box {
    max-width: var(--measure);
    margin: 0 auto;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    padding: var(--space-3) var(--space-3) var(--space-2);
    display: grid;
    gap: var(--space-2);
    transition: border-color var(--motion-fast) var(--ease);
  }
  .box:focus-within {
    border-color: var(--accent);
  }
  textarea {
    width: 100%;
    resize: none;
    border: none;
    background: transparent;
    outline: none;
    max-height: 240px;
    padding: var(--space-1) var(--space-2);
    user-select: text;
    -webkit-user-select: text;
  }
  textarea:focus-visible {
    box-shadow: none;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .context {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .chip {
    font-size: 12px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--fg);
  }
  .chip.muted {
    background: var(--bg-sunken);
    color: var(--fg-faint);
  }
</style>
