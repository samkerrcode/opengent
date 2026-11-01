<script lang="ts">
  import { agentStore } from "./agent-store.svelte";
  import {
    WEEKDAYS,
    describeOutcome,
    describePolicy,
    describeRecurrence,
    fmtWhen,
    intervalToMinutes,
    minutesToInterval,
    parseTimeOfDay,
    toDateTimeInput,
    toTimeInput,
    type IntervalUnit,
  } from "./schedule-format";
  import { nav } from "$lib/state/nav.svelte";
  import { chatStore } from "$features/chat/chat-store.svelte";
  import { describeError, type Agent, type AgentSchedule, type MissedRunPolicy, type Recurrence } from "$lib/ipc/commands";

  let { agent }: { agent: Agent } = $props();

  type Kind = Recurrence["kind"];
  type Form = {
    id: string | null;
    prompt: string;
    kind: Kind;
    onceAt: string;
    amount: number;
    unit: IntervalUnit;
    days: number[];
    time: string;
    missed: MissedRunPolicy;
    enabled: boolean;
  };

  let form = $state<Form | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const schedules = $derived(agentStore.schedulesFor(agent.id));

  function blank(): Form {
    const inAnHour = Date.now() + 3_600_000;
    return { id: null, prompt: "", kind: "weekly", onceAt: toDateTimeInput(inAnHour), amount: 1, unit: "hours", days: [0, 1, 2, 3, 4], time: "09:00", missed: "run_once", enabled: true };
  }

  function edit(s: AgentSchedule) {
    const f = blank();
    f.id = s.id;
    f.prompt = s.prompt;
    f.kind = s.recurrence.kind;
    f.missed = s.missed_policy;
    f.enabled = s.enabled;
    if (s.recurrence.kind === "once") f.onceAt = toDateTimeInput(s.recurrence.at);
    if (s.recurrence.kind === "interval") ({ amount: f.amount, unit: f.unit } = minutesToInterval(s.recurrence.minutes));
    if (s.recurrence.kind === "weekly") {
      f.days = [...s.recurrence.days];
      f.time = toTimeInput(s.recurrence.minute_of_day);
    }
    form = f;
    error = null;
  }

  function recurrence(f: Form): Recurrence {
    switch (f.kind) {
      case "once": {
        const at = new Date(f.onceAt).getTime();
        if (!Number.isFinite(at)) throw new Error("choose a date and time");
        return { kind: "once", at };
      }
      case "interval":
        return { kind: "interval", minutes: intervalToMinutes(Number(f.amount) || 0, f.unit) };
      case "weekly": {
        const minute = parseTimeOfDay(f.time);
        if (minute === null) throw new Error("choose a time of day");
        return { kind: "weekly", days: [...f.days].sort((a, b) => a - b), minute_of_day: minute };
      }
    }
  }

  async function save() {
    if (!form) return;
    busy = true;
    error = null;
    try {
      await agentStore.saveSchedule({
        id: form.id,
        agent_id: agent.id,
        prompt: form.prompt,
        recurrence: recurrence(form),
        missed_policy: form.missed,
        enabled: form.enabled,
      });
      form = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  async function act(p: Promise<unknown>) {
    error = null;
    try {
      await p;
    } catch (e) {
      error = describeError(e);
    }
  }

  async function runNow(s: AgentSchedule) {
    error = null;
    try {
      const chat = await agentStore.runNow(s.id);
      await chatStore.select(chat.id);
      nav.showChat();
    } catch (e) {
      error = describeError(e);
    }
  }

  async function openChat(id: string) {
    await chatStore.select(id);
    nav.showChat();
  }

  function toggleDay(d: number) {
    if (!form) return;
    form.days = form.days.includes(d) ? form.days.filter((x) => x !== d) : [...form.days, d];
  }
</script>

<section class="card">
  <div class="h2row">
    <h2>Schedules</h2>
    {#if !form}<button type="button" class="btn-outline btn-sm" onclick={() => ((form = blank()), (error = null))}>+ Add schedule</button>{/if}
  </div>
  <p class="hint">Runs this agent with a preset prompt. Each run opens a new chat; tool calls that need approval wait for you (and are denied after 15 minutes).</p>

  {#if schedules.length}
    <ul class="list">
      {#each schedules as s (s.id)}
        <li class="item" class:off={!s.enabled}>
          <div class="main">
            <span class="when">{describeRecurrence(s.recurrence)}</span>
            <span class="prompt" title={s.prompt}>“{s.prompt}”</span>
            <span class="sub">
              {#if s.enabled && s.next_run_at !== null}Next: {fmtWhen(s.next_run_at)}{:else if s.recurrence.kind === "once" && s.last_run_at}Finished{:else}Paused{/if}
              · {describePolicy(s.missed_policy)}
            </span>
            {#if s.last_outcome}
              <span class="sub" class:bad={s.last_outcome.kind === "failed"}>
                Last: {describeOutcome(s.last_outcome)}{#if s.last_run_at} ({fmtWhen(s.last_run_at)}){/if}
                {#if s.last_chat_id}· <button type="button" class="link" onclick={() => openChat(s.last_chat_id!)}>open chat</button>{/if}
              </span>
            {/if}
          </div>
          <div class="ops">
            <label class="toggle" title={s.enabled ? "Pause" : "Resume"}>
              <input type="checkbox" checked={s.enabled} onchange={(e) => act(agentStore.setScheduleEnabled(s.id, e.currentTarget.checked))} />
            </label>
            <button type="button" class="btn-ghost btn-sm" onclick={() => runNow(s)}>Run now</button>
            <button type="button" class="btn-ghost btn-sm" onclick={() => edit(s)}>Edit</button>
            <button type="button" class="btn-danger btn-sm" aria-label="Delete schedule" onclick={() => confirm("Delete this schedule?") && act(agentStore.removeSchedule(s.id))}>✕</button>
          </div>
        </li>
      {/each}
    </ul>
  {:else if !form}
    <p class="empty">No schedules yet.</p>
  {/if}

  {#if form}
    <div class="form">
      <label>
        <span>Prompt to send each run</span>
        <textarea rows="3" bind:value={form.prompt} maxlength="16384" placeholder="Check for new releases of my starred libraries and summarize what changed."></textarea>
      </label>
      <div class="row">
        <label class="narrow">
          <span>Repeat</span>
          <select bind:value={form.kind}>
            <option value="weekly">On days at a time</option>
            <option value="interval">Every…</option>
            <option value="once">Once</option>
          </select>
        </label>
        {#if form.kind === "weekly"}
          <div class="field">
            <span class="lbl">Days</span>
            <div class="days" role="group" aria-label="Days of the week">
              {#each WEEKDAYS as d, i (d)}
                <button type="button" class="day" class:sel={form.days.includes(i)} aria-pressed={form.days.includes(i)} onclick={() => toggleDay(i)}>{d}</button>
              {/each}
            </div>
          </div>
          <label class="narrow">
            <span>Time</span>
            <input class="native" type="time" bind:value={form.time} />
          </label>
        {:else if form.kind === "interval"}
          <label class="narrow">
            <span>Every</span>
            <input class="native" type="number" min="1" bind:value={form.amount} />
          </label>
          <label class="narrow">
            <span>&nbsp;</span>
            <select bind:value={form.unit}>
              <option value="minutes">minutes</option>
              <option value="hours">hours</option>
              <option value="days">days</option>
            </select>
          </label>
        {:else}
          <label>
            <span>At</span>
            <input class="native" type="datetime-local" bind:value={form.onceAt} />
          </label>
        {/if}
      </div>
      <fieldset class="missed">
        <legend>If the app was closed or the computer was asleep at the scheduled time</legend>
        <label class="radio"><input type="radio" bind:group={form.missed} value="run_once" /> Run it once when the app is back <small>(several missed runs count as one)</small></label>
        <label class="radio"><input type="radio" bind:group={form.missed} value="skip" /> Skip it and wait for the next scheduled time</label>
      </fieldset>
      <label class="radio"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>
      <div class="factions">
        <button type="button" class="btn-ghost btn-sm" onclick={() => (form = null)}>Cancel</button>
        <button type="button" class="btn-primary btn-sm" onclick={save} disabled={busy || !form.prompt.trim() || (form.kind === "weekly" && form.days.length === 0)}>
          {form.id ? "Save schedule" : "Add schedule"}
        </button>
      </div>
    </div>
  {/if}
  {#if error}<p class="result bad" role="alert">{error}</p>{/if}
</section>

<style>
  .card { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-elevated); padding: var(--space-4); display: grid; gap: var(--space-3); }
  h2 { margin: 0; font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted); }
  .h2row { display: flex; align-items: center; justify-content: space-between; }
  .hint { margin: 0; font-size: 12px; color: var(--fg-faint); line-height: 1.5; }
  .empty { margin: 0; font-size: 13px; color: var(--fg-faint); }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; }
  .item { display: flex; gap: var(--space-3); align-items: flex-start; border: 1px solid var(--border); border-radius: var(--radius-md); padding: 8px 10px; }
  .item.off .main { opacity: 0.6; }
  .main { display: grid; gap: 2px; min-width: 0; flex: 1; }
  .when { font-weight: 500; }
  .prompt { font-size: 12.5px; color: var(--fg-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { font-size: 11.5px; color: var(--fg-faint); }
  .sub.bad { color: var(--danger); }
  .link { padding: 0; color: var(--accent); font-size: inherit; display: inline; }
  .ops { display: flex; align-items: center; gap: 2px; flex: none; }
  .toggle input, .radio input { accent-color: var(--accent); margin: 0; }
  .toggle { padding: 0 6px; }
  .form { display: grid; gap: var(--space-3); border-top: 1px solid var(--border); padding-top: var(--space-3); }
  label { display: grid; gap: 6px; font-size: 13px; color: var(--fg-muted); }
  .lbl { font-size: 13px; color: var(--fg-muted); }
  .field { display: grid; gap: 6px; }
  .row { display: flex; flex-wrap: wrap; gap: var(--space-3); align-items: end; }
  .narrow { width: auto; }
  textarea, .native { width: 100%; padding: 8px 10px; border: 1px solid var(--border-strong); border-radius: var(--radius-md); background: var(--bg); color: var(--fg); user-select: text; -webkit-user-select: text; }
  textarea { resize: vertical; line-height: 1.5; }
  .native[type="number"] { width: 90px; }
  textarea:focus-visible, .native:focus-visible { border-color: var(--accent); box-shadow: none; }
  .days { display: flex; gap: 3px; }
  .day { padding: 6px 8px; font-size: 12px; border: 1px solid var(--border-strong); background: var(--bg); }
  .day.sel { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
  .missed { border: 1px solid var(--border); border-radius: var(--radius-md); padding: var(--space-2) var(--space-3) var(--space-3); margin: 0; display: grid; gap: 6px; }
  .missed legend { font-size: 12px; color: var(--fg-muted); padding: 0 4px; }
  .radio { display: flex; flex-direction: row; align-items: center; gap: 8px; color: var(--fg); }
  .radio small { color: var(--fg-faint); }
  .factions { display: flex; justify-content: flex-end; gap: var(--space-2); }
  .result { margin: 0; padding: 8px 10px; border-radius: var(--radius-md); font-size: 13px; }
  .result.bad { background: var(--danger-soft); color: var(--danger); }
</style>
