import { describe, expect, it } from "vitest";
import {
  describeRecurrence,
  intervalToMinutes,
  minutesToInterval,
  parseTimeOfDay,
  toTimeInput,
} from "./schedule-format";

describe("schedule-format", () => {
  it("describes recurrences", () => {
    expect(describeRecurrence({ kind: "interval", minutes: 90 })).toBe("Every 90 minutes");
    expect(describeRecurrence({ kind: "interval", minutes: 120 })).toBe("Every 2 hours");
    expect(describeRecurrence({ kind: "interval", minutes: 1440 })).toBe("Every day");
    expect(describeRecurrence({ kind: "weekly", days: [4, 0, 1, 2, 3], minute_of_day: 540 })).toMatch(/^Weekdays at /);
    expect(describeRecurrence({ kind: "weekly", days: [5, 6], minute_of_day: 0 })).toMatch(/^Weekends at /);
    expect(describeRecurrence({ kind: "weekly", days: [0, 2], minute_of_day: 0 })).toMatch(/^Mon, Wed at /);
  });

  it("round-trips times and intervals", () => {
    expect(parseTimeOfDay("09:30")).toBe(570);
    expect(parseTimeOfDay("24:00")).toBeNull();
    expect(toTimeInput(570)).toBe("09:30");
    expect(intervalToMinutes(3, "hours")).toBe(180);
    expect(minutesToInterval(180)).toEqual({ amount: 3, unit: "hours" });
    expect(minutesToInterval(45)).toEqual({ amount: 45, unit: "minutes" });
  });
});
