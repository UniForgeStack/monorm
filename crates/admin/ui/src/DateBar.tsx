import { useQuery } from "@tanstack/react-query";
import { ChevronRight } from "lucide-react";
import * as api from "./api";
import { boundsOf, edgesOf, periodsOf, readRange, viewOf, writeRange, type Period } from "./dates";
import { type Zone } from "./format";
import { Button } from "./ui";

type Change = (edit: (p: URLSearchParams) => void, keepPosition?: boolean) => void;

/** The drill-down over the list: years, then the months of a year, then the days of a month. A click writes the range filter of the field. */
export function DateBar({ resource, meta, params, change, zone }: { resource: string; meta: api.Resource; params: URLSearchParams; change: Change; zone: Zone }) {
  const hierarchy = meta.date_hierarchy;
  if (!hierarchy) return null;
  return <Bar resource={resource} field={hierarchy.field} kind={hierarchy.kind} counts={hierarchy.counts} params={params} change={change} zone={zone} />;
}

function Bar({ resource, field, kind, counts, params, change, zone }: { resource: string; field: string; kind: "date" | "datetime"; counts: boolean; params: URLSearchParams; change: Change; zone: Zone }) {
  const range = readRange(params, field, kind, zone);
  const view = viewOf(range.from, range.to);
  const search = new URLSearchParams([...params].filter(([k]) => k === "q" || k === "without" || k.startsWith("f.") || k.startsWith("n."))).toString();
  // A level with a closed range needs no statement for its periods; the year level and an open range ask for the first and last value.
  const open = view.level !== "single" && (!range.from || !range.to);
  const bounds = useQuery({ queryKey: ["dates", resource, "range", search, zone], queryFn: () => api.dates(resource, search ? `?${search}` : ""), enabled: open, placeholderData: (prev) => prev, retry: false });
  const limits = open ? boundsOf(bounds.data ?? {}, kind, zone) : null;
  const periods = open && !limits ? [] : periodsOf(view, limits);
  const edges = edgesOf(periods, kind, zone, bounds.data?.min ?? null);
  const wanted = counts && periods.length > 0;
  const tally = useQuery({ queryKey: ["dates", resource, "counts", search, edges.join(",")], queryFn: () => api.dates(resource, search ? `?${search}` : "", edges), enabled: wanted, placeholderData: (prev) => prev, retry: false });
  const numbers = wanted && tally.data?.counts?.length === periods.length ? tally.data.counts : null;
  const items: [Period, number | null][] = periods.map((p, i) => [p, numbers ? (numbers[i] ?? 0) : null]);
  // A period with no row is hidden once the counts are known.
  const shown = counts && numbers ? items.filter(([, n]) => n !== 0) : items;
  const go = (from: string | null, to: string | null) => change((p) => writeRange(p, field, kind, zone, from, to));
  if (view.level === "year" && shown.length === 0) return null;
  return (
    <div className="mb-3 flex flex-wrap items-center gap-x-4 gap-y-2" role="group" aria-label="Date drill-down">
      <nav aria-label="Date breadcrumb" className="flex items-center gap-1 text-sm">
        {view.crumbs.map((c, i) => {
          const last = i === view.crumbs.length - 1;
          return (
            <span key={c.label + i} className="flex items-center gap-1">
              {i > 0 && <ChevronRight aria-hidden="true" className="size-3.5 text-muted-foreground" />}
              {last && i > 0 ? <span aria-current="page" className="font-medium">{c.label}</span> : <Button variant="plain" size="sm" onClick={() => go(c.from, c.to)}>{c.label}</Button>}
            </span>
          );
        })}
      </nav>
      <ul className="m-0 flex list-none flex-wrap gap-1 p-0">
        {shown.map(([p, n]) => (
          <li key={p.label}>
            <Button variant="plain" size="sm" onClick={() => go(p.from, p.to)}>
              {p.label}
              {n !== null && <span className="ml-1 text-muted-foreground">{n.toLocaleString("en-US")}</span>}
            </Button>
          </li>
        ))}
      </ul>
    </div>
  );
}
