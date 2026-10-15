import { Decimal } from "./decimal.js";

/** A key for comparing and grouping values: `Date` and `Decimal` compare by value. */
export function key(v: unknown): unknown {
  if (v instanceof Date) {
    return `d${v.getTime()}`;
  }
  if (v instanceof Decimal) {
    return `n${v.toString()}`;
  }
  return v;
}
