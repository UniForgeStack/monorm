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

/** The text that the input of a field starts with, from the value that the server sent. */
export function toInput(value: unknown, w: string, zone: Zone): Input {
  if (w === "checkbox") return value === true;
  if (value === null || value === undefined) return "";
  if (w === "json" || w === "list") return JSON.stringify(value);
  if (w === "datetime" && typeof value === "string") {
    const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    if (Number.isNaN(d.getTime())) return value;
    if (zone === "utc") return d.toISOString().slice(0, 19);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
  return String(value);
}

const STRINGS = ["text", "textarea", "select", "radio", "email", "url"];

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
    try {
      return { value: JSON.parse(text) };
    } catch {
      return { error: w === "list" ? "Must be a JSON list, like [1, 2]" : "Must be valid JSON" };
    }
  }
  if (w === "datetime") return { value: zone === "local" ? new Date(text).toISOString() : text };
  return { value: text };
}

export const inputType = (w: string) => (w === "datetime" ? "datetime-local" : w === "date" || w === "email" || w === "url" ? w : "text");
