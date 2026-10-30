<script lang="ts">
  import ModelPicker from "$features/providers/ModelPicker.svelte";

  let {
    busy,
    onsend,
    onstop,
    onopensettings,
  }: { busy: boolean; onsend: (text: string) => void; onstop: () => void; onopensettings: () => void } = $props();

  let value = $state("");
  let textarea = $state<HTMLTextAreaElement | null>(null);

  export function focus() {
    textarea?.focus();
  }

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

  $effect(() => {
    void value;
    if (textarea) {
      textarea.style.height = "auto";
      textarea.style.height = `${Math.min(textarea.scrollHeight, 220)}px`;
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
      placeholder="Message… (Shift+Enter for a new line)"
      onkeydown={onKeydown}
      autocomplete="off"
      spellcheck="true"
    ></textarea>
    <div class="row">
      <div class="context">
        <ModelPicker {onopensettings} />
        <span class="chip" title="File attachments arrive in Phase 1">⌁ Attach</span>
        <span class="chip" title="MCP tools arrive in Phase 3">⚙ Tools</span>
      </div>
      {#if busy}
        <button type="button" class="btn-outline stop" onclick={onstop} title="Stop (Esc)">
          <span class="stop-icon" aria-hidden="true"></span> Stop
        </button>
      {:else}
        <button type="submit" class="btn-primary send" disabled={!value.trim()} title="Send (Enter)">
          Send <span aria-hidden="true">↵</span>
        </button>
      {/if}
    </div>
  </div>
  <p class="hint">Responses can be wrong. Check important details.</p>
</form>

<style>
  .composer {
    padding: 0 var(--space-6) var(--space-3);
    background: linear-gradient(to bottom, transparent, var(--bg) 30%);
  }
  .box {
    max-width: var(--measure);
    margin: 0 auto;
    background: var(--bg-elevated);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xl);
    padding: var(--space-3) var(--space-3) var(--space-2);
    display: grid;
    gap: var(--space-2);
    box-shadow: var(--shadow-1);
    transition: border-color var(--motion-fast) var(--ease), box-shadow var(--motion-fast) var(--ease);
  }
  .box:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  textarea {
    width: 100%;
    resize: none;
    border: none;
    background: transparent;
    outline: none;
    max-height: 220px;
    padding: var(--space-1) var(--space-2);
    font-size: 15px;
    line-height: 1.5;
    user-select: text;
    -webkit-user-select: text;
  }
  textarea::placeholder {
    color: var(--fg-faint);
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
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
    min-width: 0;
  }
  .send,
  .stop {
    border-radius: 999px;
    padding: 6px 14px;
  }
  .stop-icon {
    width: 9px;
    height: 9px;
    background: currentColor;
    border-radius: 2px;
  }
  .hint {
    max-width: var(--measure);
    margin: var(--space-2) auto 0;
    text-align: center;
    font-size: 11.5px;
    color: var(--fg-faint);
  }
</style>
