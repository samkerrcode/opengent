// Human-readable schedule descriptions and form <-> Recurrence conversion. Pure (no Svelte)
// so it is unit-testable. Weekdays follow Rust: 0 = Monday … 6 = Sunday.
import type { MissedRunPolicy, Recurrence, ScheduleOutcome } from "$lib/ipc/commands";

export const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;

export function fmtTimeOfDay(minuteOfDay: number): string {
  const d = new Date(2000, 0, 1, Math.floor(minuteOfDay / 60), minuteOfDay % 60);
  return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function parseTimeOfDay(value: string): number | null {
  const m = /^(\d{1,2}):(\d{2})/.exec(value);
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return h < 24 && min < 60 ? h * 60 + min : null;
}

export function toTimeInput(minuteOfDay: number): string {
  return `${String(Math.floor(minuteOfDay / 60)).padStart(2, "0")}:${String(minuteOfDay % 60).padStart(2, "0")}`;
}

/** `<input type="datetime-local">` value for an epoch-ms time (local). */
export function toDateTimeInput(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

export function fmtWhen(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const sameDay = d.toDateString() === new Date(now).toDateString();
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (sameDay) return `today ${time}`;
  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  if (d.toDateString() === tomorrow.toDateString()) return `tomorrow ${time}`;
  return `${d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" })} ${time}`;
}

function fmtInterval(minutes: number): string {
  if (minutes % 1440 === 0) return minutes === 1440 ? "Every day" : `Every ${minutes / 1440} days`;
  if (minutes % 60 === 0) return minutes === 60 ? "Every hour" : `Every ${minutes / 60} hours`;
  return minutes === 1 ? "Every minute" : `Every ${minutes} minutes`;
}

function fmtDays(days: number[]): string {
  const set = [...new Set(days)].sort((a, b) => a - b);
  const key = set.join(",");
  if (key === "0,1,2,3,4,5,6") return "Every day";
  if (key === "0,1,2,3,4") return "Weekdays";
  if (key === "5,6") return "Weekends";
  return set.map((d) => WEEKDAYS[d]).join(", ");
}

export function describeRecurrence(r: Recurrence): string {
  switch (r.kind) {
    case "once":
      return `Once, ${fmtWhen(r.at)}`;
    case "interval":
      return fmtInterval(r.minutes);
    case "weekly":
      return `${fmtDays(r.days)} at ${fmtTimeOfDay(r.minute_of_day)}`;
  }
}

export function describePolicy(p: MissedRunPolicy): string {
  return p === "run_once" ? "Missed runs: run once when back" : "Missed runs: skipped";
}

export function describeOutcome(o: ScheduleOutcome): string {
  switch (o.kind) {
    case "ran":
      return "Ran on schedule";
    case "manual":
      return "Ran manually";
    case "caught_up":
      return `Caught up a run missed ${fmtWhen(o.missed_at)}`;
    case "skipped":
      return `Skipped a run missed ${fmtWhen(o.missed_at)}`;
    case "failed":
      return `Failed to start: ${o.message}`;
  }
}

export type IntervalUnit = "minutes" | "hours" | "days";
const UNIT_MINUTES: Record<IntervalUnit, number> = { minutes: 1, hours: 60, days: 1440 };

export function intervalToMinutes(amount: number, unit: IntervalUnit): number {
  return Math.round(amount * UNIT_MINUTES[unit]);
}

export function minutesToInterval(minutes: number): { amount: number; unit: IntervalUnit } {
  if (minutes % 1440 === 0) return { amount: minutes / 1440, unit: "days" };
  if (minutes % 60 === 0) return { amount: minutes / 60, unit: "hours" };
  return { amount: minutes, unit: "minutes" };
}
