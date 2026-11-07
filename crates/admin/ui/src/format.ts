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
