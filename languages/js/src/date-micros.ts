/** The hidden properties of a `Date` from a `DateTime` column (set by the native driver):
 * the microseconds below its milliseconds, and the milliseconds they belong to. */
export const MICROS = "monorm:micros";
export const MICROS_MS = "monorm:micros-ms";

/** The microseconds below the milliseconds of `d`, unless its time changed after they were set. */
export function extraMicros(d: Date): number {
  return Reflect.get(d, MICROS_MS) === d.getTime() ? Number(Reflect.get(d, MICROS) ?? 0) : 0;
}

/** Keeps `us` (0 to 999) below the milliseconds of `d`. */
export function keepMicros(d: Date, us: number): void {
  if (!us) return;
  Object.defineProperty(d, MICROS, { value: us });
  Object.defineProperty(d, MICROS_MS, { value: d.getTime() });
}

/** UTC time-of-day with all six stored fractional digits. */
export function timeText(d: Date): string {
  return d.toISOString().slice(11, 23) + String(extraMicros(d)).padStart(3, "0");
}

export function parseTime(text: string): Date | undefined {
  const m = /^(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,6}))?$/.exec(text);
  if (!m || +m[1]! > 23 || +m[2]! > 59 || +m[3]! > 59) return undefined;
  const us = +(m[4] ?? "").padEnd(6, "0");
  const ms = ((+m[1]! * 60 + +m[2]!) * 60 + +m[3]!) * 1000 + Math.floor(us / 1000);
  const d = new Date(ms);
  keepMicros(d, us % 1000);
  return d;
}
