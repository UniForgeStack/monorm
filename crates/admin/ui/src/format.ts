import type { Column } from "./api";

export type Zone = "local" | "utc";

const KEY = "monorm-admin-zone";

export const getZone = (): Zone => (localStorage.getItem(KEY) === "utc" ? "utc" : "local");
export const setZone = (z: Zone) => localStorage.setItem(KEY, z);

/** The value of an RFC 3339 time in the browser time zone (or UTC when the user chose it). A time with no offset is UTC. */
export function dateTime(value: string, zone: Zone): string {
  const text = /(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`;
  const date = new Date(text);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "long",
    timeZone: zone === "utc" ? "UTC" : undefined,
  }).format(date);
}

/** One cell as text. A `Date` (no time of day) has no zone and shows as the server gave it. */
export function cell(value: unknown, column: Column, zone: Zone): string {
  if (value === null || value === undefined) return "";
  if (Array.isArray(value)) return value.map((v) => cell(v, { ...column, array: false }, zone)).join(", ");
  if (column.widget === "checkbox") return value ? "Yes" : "No";
  if (column.widget === "datetime" && typeof value === "string") return dateTime(value, zone);
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

export type Input = string | boolean;

const pad = (n: number) => String(n).padStart(2, "0");

/** The widget that a control uses: a `time` column has the `text` widget on the server, and the browser has a picker for it. */
export const widgetFor = (widget: string, column: Column) => (widget === "text" && column.type === "time" && !column.array ? "time" : widget);

/** The text that the input of a field starts with, from the value that the server sent. Settled, so an untouched picker reads back as it started. */
export const toInput = (value: unknown, w: string, zone: Zone): Input => settled(startText(value, w, zone), w);

function startText(value: unknown, w: string, zone: Zone): Input {
  if (w === "checkbox") return value === true;
  if (value === null || value === undefined) return "";
  if (w === "json") return JSON.stringify(value, null, 2);
  if (w === "list") return JSON.stringify(value);
  if (w === "datetime" && typeof value === "string") {
    const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    if (Number.isNaN(d.getTime())) return value;
    if (zone === "utc") return d.toISOString().slice(0, 19);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
  // A time of day has no zone: the picker shows what the server holds, to the second.
  if (w === "time" && typeof value === "string") return value.replace(/^(\d\d:\d\d:\d\d)\.\d+$/, "$1");
  return String(value);
}

const STRINGS = ["text", "textarea", "select", "radio", "email", "url"];
const DATE = /^(\d{4})-(\d\d)-(\d\d)$/;
const DATE_TIME = /^(\d{4})-(\d\d)-(\d\d)[T ](\d\d):(\d\d)(?::(\d\d)(\.\d+)?)?$/;
const TIME = /^(\d\d):(\d\d)(?::(\d\d)(\.\d+)?)?$/;

/** True when the calendar has this day (no 31 February). `setUTCFullYear` keeps the years 0 to 99: `Date.UTC` reads them as 1900 to 1999. */
const real = (y: number, m: number, d: number) => {
  const probe = new Date(0);
  probe.setUTCFullYear(y, m - 1, d);
  return probe.getUTCFullYear() === y && probe.getUTCMonth() === m - 1 && probe.getUTCDate() === d;
};

/** The one JSON check of the admin: the parsed value, or the message of the parser. */
export function parseJson(text: string): { value: unknown } | { error: string } {
  try {
    return { value: JSON.parse(text) };
  } catch (e) {
    return { error: e instanceof Error ? e.message : "Not valid JSON" };
  }
}

/** The text of a picker as the browser reads it back: a picker drops zero seconds, so `08:00:00` and `08:00` are one value. */
export function settled(input: Input, w: string): Input {
  if (typeof input !== "string" || !stepsSeconds(w)) return input;
  return input.replace(/(\d\d:\d\d):00$/, "$1");
}

/** The value to send for the text of an input, or a message when the text cannot be a value of the field. */
export function fromInput(input: Input, w: string, nullable: boolean, zone: Zone): { value: unknown } | { error: string } {
  if (typeof input === "boolean") return { value: input };
  const text = input.trim();
  if (STRINGS.includes(w)) {
    if (text === "" && nullable) return { value: null };
    return { value: w === "text" || w === "textarea" ? input : text };
  }
  if (text === "") return { value: nullable ? null : "" };
  if (w === "json" || w === "list") {
    const parsed = parseJson(text);
    return "error" in parsed ? { error: w === "list" ? "Must be a JSON list, like [1, 2]" : "Must be valid JSON" } : parsed;
  }
  if (w === "date") {
    const m = DATE.exec(text);
    return m && real(Number(m[1]), Number(m[2]), Number(m[3])) ? { value: text } : { error: "Must be a date, like 2026-01-31" };
  }
  if (w === "time") {
    const m = TIME.exec(text);
    if (!m || Number(m[1]) > 23 || Number(m[2]) > 59 || Number(m[3] ?? 0) > 59) return { error: "Must be a time, like 14:30:00" };
    return { value: `${m[1]}:${m[2]}:${m[3] ?? "00"}${m[4] ?? ""}` };
  }
  if (w === "datetime") {
    const m = DATE_TIME.exec(text);
    const n = (i: number) => Number(m?.[i] ?? 0);
    const [y, mo, d, h, mi, s] = [n(1), n(2), n(3), n(4), n(5), n(6)] as [number, number, number, number, number, number];
    if (!m || !real(y, mo, d) || h > 23 || mi > 59 || s > 59) return { error: "Must be a date and time, like 2026-01-31 14:30" };
    if (zone === "utc") return { value: `${m[1]}-${m[2]}-${m[3]}T${m[4]}:${m[5]}:${m[6] ?? "00"}${m[7] ?? ""}Z` };
    // The browser reads the parts in its own zone: the same moment as the picker shows.
    return { value: new Date(y, mo - 1, d, h, mi, s).toISOString() };
  }
  return { value: text };
}

export const inputType = (w: string) => (w === "datetime" ? "datetime-local" : w === "date" || w === "time" || w === "email" || w === "url" ? w : "text");

/** True for the controls that the browser draws as a picker: they take a seconds step so the value keeps its seconds. */
export const stepsSeconds = (w: string) => w === "datetime" || w === "time";
