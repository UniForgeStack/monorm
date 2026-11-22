// Unit tests of the date drill-down rules (src/dates.ts): the level of a range, the periods and the edges, in the local zone and in UTC.
import assert from "node:assert/strict";
import { test } from "node:test";

import { boundsOf, edgesOf, periodsOf, readRange, viewOf, writeRange } from "../src/dates.ts";

// Europe/Berlin changes to summer time on 2026-03-29: that day has 23 hours.
process.env.TZ = "Europe/Berlin";

const labels = (periods) => periods.map((p) => p.label);

test("the level comes from the range: none, a year, a month, a day or a free range", () => {
  assert.equal(viewOf(null, null).level, "year");
  assert.deepEqual(viewOf(null, null).crumbs.map((c) => c.label), ["All dates"]);
  const year = viewOf("2025-01-01", "2025-12-31");
  assert.equal(year.level, "month");
  assert.deepEqual(year.crumbs.map((c) => c.label), ["All dates", "2025"]);
  const month = viewOf("2025-03-01", "2025-03-31");
  assert.equal(month.level, "day");
  assert.deepEqual(month.crumbs.map((c) => c.label), ["All dates", "2025", "March"]);
  assert.equal(viewOf("2024-02-01", "2024-02-29").level, "day", "February of a leap year ends on the 29th");
  const one = viewOf("2025-03-14", "2025-03-14");
  assert.equal(one.level, "single");
  assert.deepEqual(one.crumbs.map((c) => c.label), ["All dates", "2025", "March", "14"]);
  assert.equal(viewOf("2025-03-10", "2025-04-09").level, "day", "31 days or fewer: days");
  assert.equal(viewOf("2025-03-10", "2025-04-10").level, "month", "32 days: months");
  assert.equal(viewOf("2025-03-10", null).level, "month", "an open range: months");
});

test("the year level shows at most 50 years and an older period", () => {
  assert.deepEqual(labels(periodsOf(viewOf(null, null), { min: "2023-05-01", max: "2026-01-02" })), ["2023", "2024", "2025", "2026"]);
  const many = periodsOf(viewOf(null, null), { min: "1950-01-01", max: "2026-01-02" });
  assert.equal(many.length, 51);
  assert.deepEqual([many[0].label, many[0].from, many[0].to, many[1].label], ["older", null, "1976-12-31", "1977"]);
  assert.equal(many.at(-1).label, "2026");
});

test("the month and day levels list every month of the year and every day of the month", () => {
  assert.equal(periodsOf(viewOf("2025-01-01", "2025-12-31"), null).length, 12);
  assert.deepEqual(labels(periodsOf(viewOf("2025-01-01", "2025-12-31"), null)).slice(0, 2), ["January", "February"]);
  assert.equal(periodsOf(viewOf("2024-02-01", "2024-02-29"), null).length, 29);
  assert.deepEqual(periodsOf(viewOf("2025-03-14", "2025-03-14"), null), []);
  const free = periodsOf(viewOf("2024-11-20", null), { min: "2024-01-01", max: "2025-02-10" });
  assert.deepEqual(labels(free), ["November 2024", "December 2024", "January 2025", "February 2025"]);
  const long = periodsOf(viewOf("2000-01-01", "2026-01-01"), null);
  assert.equal(long.length, 27, "more than 60 months: years");
  assert.deepEqual(labels(periodsOf(viewOf("2020-01-01", null), null)), []);
});

test("the edges are the start of each period in the zone, across a daylight saving change", () => {
  const march = periodsOf(viewOf("2026-03-01", "2026-03-31"), null);
  const local = edgesOf(march, "datetime", "local", null);
  assert.equal(local.length, 32);
  assert.equal(local[0], "2026-02-28T23:00:00Z");
  assert.equal(local[28], "2026-03-28T23:00:00Z");
  assert.equal(local[29], "2026-03-29T22:00:00Z", "March 29 has 23 hours in Berlin: its edges are 23 hours apart");
  assert.equal(local[30], "2026-03-30T22:00:00Z", "from the 30th the offset is +02:00");
  assert.equal(local[31], "2026-03-31T22:00:00Z");
  const utc = edgesOf(march, "datetime", "utc", null);
  assert.deepEqual([utc[0], utc[29], utc[31]], ["2026-03-01T00:00:00Z", "2026-03-30T00:00:00Z", "2026-04-01T00:00:00Z"]);
  const months = edgesOf(periodsOf(viewOf("2026-01-01", "2026-12-31"), null), "datetime", "local", null);
  assert.equal(months.length, 13);
  assert.deepEqual([months[0], months[2], months[3]], ["2025-12-31T23:00:00Z", "2026-02-28T23:00:00Z", "2026-03-31T22:00:00Z"]);
  assert.deepEqual(edgesOf(periodsOf(viewOf("2026-02-01", "2026-02-28"), null).slice(0, 2), "date", "local", null), ["2026-02-01", "2026-02-02", "2026-02-03"]);
});

test("an older period starts at the first value of the field", () => {
  const periods = periodsOf(viewOf(null, null), { min: "1950-01-01", max: "2026-01-02" });
  const edges = edgesOf(periods, "datetime", "utc", "1950-01-01T00:00:00+00:00");
  assert.equal(edges[0], "1950-01-01T00:00:00+00:00");
  assert.equal(edges[1], "1977-01-01T00:00:00Z");
  assert.equal(edges.length, periods.length + 1);
});

test("the bounds of the dates answer are calendar days in the zone", () => {
  const range = { min: "2025-12-31T23:30:00+00:00", max: "2026-01-02T03:04:05+00:00" };
  assert.deepEqual(boundsOf(range, "datetime", "local"), { min: "2026-01-01", max: "2026-01-02" });
  assert.deepEqual(boundsOf(range, "datetime", "utc"), { min: "2025-12-31", max: "2026-01-02" });
  assert.deepEqual(boundsOf({ min: "2025-12-31", max: "2026-01-02" }, "date", "local"), { min: "2025-12-31", max: "2026-01-02" });
  assert.equal(boundsOf({ min: null, max: null }, "datetime", "utc"), null);
});

test("a click writes the same parameters as the range filter and the URL reads back as the same days", () => {
  const p = new URLSearchParams("f.at:gte=old&q=x");
  writeRange(p, "at", "datetime", "local", "2026-03-01", "2026-03-31");
  assert.equal(p.get("f.at:gte"), "2026-02-28T23:00:00Z");
  assert.equal(p.get("f.at:lt"), "2026-03-31T22:00:00Z");
  assert.equal(p.get("q"), "x");
  assert.deepEqual(readRange(p, "at", "datetime", "local"), { from: "2026-03-01", to: "2026-03-31" });
  writeRange(p, "at", "datetime", "local", null, "2025-12-31");
  assert.equal(p.has("f.at:gte"), false, "an older period has no lower bound");
  const day = new URLSearchParams();
  writeRange(day, "joined", "date", "local", "2026-03-01", "2026-03-31");
  assert.equal(day.get("f.joined:gte"), "2026-03-01");
  assert.equal(day.get("f.joined:lte"), "2026-03-31", "a date field ends on the last day");
  assert.deepEqual(readRange(day, "joined", "date", "local"), { from: "2026-03-01", to: "2026-03-31" });
});
