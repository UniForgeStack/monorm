import { Decimal } from "decimal.js";

/** Adds decimal strings without binary rounding; the package declares `decimal.js` as a dependency. */
export function sum(values: string[]): string {
  return values.reduce((total, value) => total.plus(value), new Decimal(0)).toString();
}
