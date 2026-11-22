// The date drill-down of a list: which level the URL range shows, the periods of that level and the edges of the count request.
// Pure functions of calendar days (`YYYY-MM-DD`) in a zone, so a unit test can run them without a browser.
import type { Zone } from "./format";

export type Kind = "date" | "datetime";
export type Level = "year" | "month" | "day" | "single";

/** The most years of the year level: the years before them are one `older` period. */
export const MAX_YEARS = 50;
/** The server takes at most 61 edges: 60 periods. */
const MAX_PERIODS = 60;

const pad = (n: number) => String(n).padStart(2, "0");

export const dayOf = (d: Date, zone: Zone) =>
  zone === "utc" ? `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}` : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

const parts = (day: string) => day.split("-").map(Number) as [number, number, number];

/** The instant at which a calendar day starts in the zone, as RFC 3339 in UTC. */
export function startOf(day: string, zone: Zone): string {
  const [y, m, d] = parts(day);
  const at = zone === "utc" ? new Date(Date.UTC(y, m - 1, d)) : new Date(y, m - 1, d);
  return at.toISOString().replace(/\.\d{3}Z$/, "Z");
}

export const plusDays = (day: string, n: number) => {
  const [y, m, d] = parts(day);
  return dayOf(new Date(Date.UTC(y, m - 1, d + n)), "utc");
};

const daysBetween = (from: string, to: string) => {
  const [a, b] = [parts(from), parts(to)];
  return Math.round((Date.UTC(b[0], b[1] - 1, b[2]) - Date.UTC(a[0], a[1] - 1, a[2])) / 86_400_000);
};

const lastOfMonth = (y: number, m: number) => dayOf(new Date(Date.UTC(y, m, 0)), "utc");
const MONTH = new Intl.DateTimeFormat("en-US", { month: "long", timeZone: "UTC" });
const monthName = (m: number) => MONTH.format(Date.UTC(2000, m - 1, 1));

const bound = (op: string, name: string) => `f.${name}:${op}`;

/** The range of the field in the URL as calendar days in the zone, `to` the last day. A missing side is null. */
export function readRange(params: URLSearchParams, name: string, kind: Kind, zone: Zone): { from: string | null; to: string | null } {
  const gte = params.get(bound("gte", name));
  if (kind === "date") return { from: gte, to: params.get(bound("lte", name)) };
  const lt = params.get(bound("lt", name));
  const asDay = (v: string) => dayOf(new Date(v), zone);
  const from = gte && !Number.isNaN(new Date(gte).getTime()) ? asDay(gte) : null;
  const to = lt && !Number.isNaN(new Date(lt).getTime()) ? plusDays(asDay(lt), -1) : null;
  return { from, to };
};

/** Writes a range of calendar days (`to` is the last day) as the same two parameters that the range filter writes. A null side is dropped. */
export function writeRange(p: URLSearchParams, name: string, kind: Kind, zone: Zone, from: string | null, to: string | null) {
  const date = kind === "date";
  const lo = bound("gte", name);
  const hi = bound(date ? "lte" : "lt", name);
  p.delete(lo);
  p.delete(hi);
  if (from) p.set(lo, date ? from : startOf(from, zone));
  if (to) p.set(hi, date ? to : startOf(plusDays(to, 1), zone));
}

export interface Crumb {
  label: string;
  /** The range that a click on the crumb writes; null both: all dates. */
  from: string | null;
  to: string | null;
}

export interface View {
  level: Level;
  crumbs: Crumb[];
  from: string | null;
  to: string | null;
}

/** The level of a range: no range gives years, a year gives its months, a month gives its days, a day gives the breadcrumb only. */
export function viewOf(from: string | null, to: string | null): View {
  const all: Crumb = { label: "All dates", from: null, to: null };
  if (!from && !to) return { level: "year", crumbs: [all], from, to };
  const free = (level: Level): View => ({ level, crumbs: [all], from, to });
  if (!from || !to) return free("month");
  const [y, m, d] = parts(from);
  const [y2, m2] = parts(to);
  const year: Crumb = { label: String(y), from: `${y}-01-01`, to: `${y}-12-31` };
  const month: Crumb = { label: monthName(m), from: `${y}-${pad(m)}-01`, to: lastOfMonth(y, m) };
  if (y === y2 && from === year.from && to === year.to) return { level: "month", crumbs: [all, year], from, to };
  if (y === y2 && m === m2 && from === month.from && to === month.to) return { level: "day", crumbs: [all, year, month], from, to };
  if (from === to) return { level: "single", crumbs: [all, year, month, { label: String(d), from, to }], from, to };
  return free(daysBetween(from, to) < 31 ? "day" : "month");
}

export interface Period {
  label: string;
  /** The first and last day; a null `from` is the `older` period. */
  from: string | null;
  to: string;
}

function years(first: number, last: number, older: boolean): Period[] {
  const start = Math.max(first, last - MAX_YEARS + 1);
  const out: Period[] = [];
  if (older && start > first) out.push({ label: "older", from: null, to: `${start - 1}-12-31` });
  for (let y = start; y <= last; y++) out.push({ label: String(y), from: `${y}-01-01`, to: `${y}-12-31` });
  return out;
}

function months(from: string, to: string): Period[] {
  const [y, m] = parts(from);
  const [y2, m2] = parts(to);
  const out: Period[] = [];
  for (let at = y * 12 + m - 1; at <= y2 * 12 + m2 - 1; at++) {
    const [yy, mm] = [Math.floor(at / 12), (at % 12) + 1];
    out.push({ label: `${monthName(mm)} ${yy}`, from: `${yy}-${pad(mm)}-01`, to: lastOfMonth(yy, mm) });
  }
  return out;
}

/**
 * The periods of a view. `bounds` are the first and last day with a value (from the min and max): they close the open side of a range.
 * A level that would need more than 60 periods shows years instead.
 */
export function periodsOf(view: View, bounds: { min: string; max: string } | null): Period[] {
  if (view.level === "single") return [];
  const from = view.from ?? bounds?.min;
  const to = view.to ?? bounds?.max;
  if (!from || !to || from > to) return [];
  if (view.level === "year") return years(parts(from)[0], parts(to)[0], true);
  if (view.level === "day") {
    const out: Period[] = [];
    for (let day = from; day <= to; day = plusDays(day, 1)) out.push({ label: String(parts(day)[2]), from: day, to: day });
    return out;
  }
  const list = months(from, to);
  if (list.length <= MAX_PERIODS) return list.map((p) => ({ ...p, label: view.crumbs.length > 1 ? monthName(parts(p.from as string)[1]) : p.label }));
  return years(parts(from)[0], parts(to)[0], false);
}

/** The edges of the periods for the count request: the start of each period and the end of the last. `min` starts an `older` period. */
export function edgesOf(periods: Period[], kind: Kind, zone: Zone, min: string | null): string[] {
  const edge = (day: string) => (kind === "date" ? day : startOf(day, zone));
  const out = periods.map((p) => (p.from ? edge(p.from) : (min ?? "")));
  const last = periods[periods.length - 1];
  if (last) out.push(edge(plusDays(last.to, 1)));
  return out;
}

/** The first and last day of the answer of the `dates` route, as calendar days in the zone. */
export function boundsOf(range: { min?: string | null; max?: string | null }, kind: Kind, zone: Zone): { min: string; max: string } | null {
  if (!range.min || !range.max) return null;
  const asDay = (v: string) => (kind === "date" ? v : dayOf(new Date(v), zone));
  return { min: asDay(range.min), max: asDay(range.max) };
}
