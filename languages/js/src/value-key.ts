import { Decimal } from "./decimal.js";
import { Duration } from "./duration.js";

/** A key for comparing and grouping values: `Date`, `Decimal` and `Duration` compare by value. */
export function key(v: unknown): unknown {
  if (v instanceof Date) {
    return `d${v.getTime()}`;
  }
  if (v instanceof Decimal) {
    return `n${v.toString()}`;
  }
  if (v instanceof Duration) {
    return `u${v.micros}`;
  }
  return v;
}
