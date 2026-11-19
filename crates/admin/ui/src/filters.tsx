import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Filter, X } from "lucide-react";
import { useState } from "react";
import * as api from "./api";
import { Badge } from "./components/ui/badge";
import { Input, Select } from "./components/ui/input";
import { Collapsible } from "./components/ui/collapsible";
import { Popover } from "./components/ui/popover";
import { type Zone } from "./format";
import { Button } from "./ui";

/** The keys of a list URL that hold a position, not a view: any change of the view drops them. */
export const POSITION = ["cursor", "before", "last", "page"];

type Params = URLSearchParams;
type Change = (edit: (p: Params) => void) => void;

const key = (f: api.Filter, op = "") => `f.${f.name}${op ? `:${op}` : ""}`;
const isFilterKey = (k: string) => k.startsWith("f.") || k.startsWith("n.");
const legend = "mb-1 p-0 text-xs font-medium text-muted-foreground";
const small = "h-8 w-auto min-w-0 px-2";

const safeDecode = (v: string) => {
  try {
    return decodeURIComponent(v);
  } catch {
    return v;
  }
};

/** True when the list has at least one filter control. */
export const hasFilters = (meta: api.Resource) => meta.filters.length > 0 || meta.named_filters.length > 0;

/** The filters in a popover: named filters first, then one control per filter. Every control writes to the URL parameters. */
export function FilterPopover({ meta, params, change, zone }: { meta: api.Resource; params: Params; change: Change; zone: Zone }) {
  if (!hasFilters(meta)) return null;
  const active = [...new Set([...params.keys()].filter(isFilterKey))].length;
  return (
    <Popover
      label="Filters"
      trigger={
        <Button variant="plain">
          <Filter aria-hidden="true" />
          Filters
          {active > 0 && <Badge variant="default" className="px-1.5 py-0">{active}</Badge>}
        </Button>
      }
    >
      <div className="grid gap-4 sm:grid-cols-2">
        {meta.named_filters.map((n) => (
          <Named key={n.name} filter={n} params={params} change={change} />
        ))}
        {meta.filters.map((f) => (
          <Control key={f.name} filter={f} params={params} change={change} zone={zone} />
        ))}
      </div>
      {active > 0 && (
        <div className="mt-4 border-t border-border pt-3">
          <Button variant="plain" size="sm" onClick={() => change(clearFilters)}>
            Clear filters
          </Button>
        </div>
      )}
    </Popover>
  );
}

const clearFilters = (p: Params) => {
  for (const k of [...p.keys()]) if (isFilterKey(k)) p.delete(k);
};

interface Chip {
  id: string;
  label: string;
  /** A relation value: the chip shows the name of the record. */
  relation?: { target: string; value: string };
  drop: (p: Params) => void;
}

/** The active filters as one chip each (a relation filter has one chip for each chosen record). */
export function activeChips(meta: api.Resource, params: Params, zone: Zone): Chip[] {
  const out: Chip[] = [];
  for (const k of new Set(params.keys())) {
    if (k.startsWith("n.")) {
      const f = meta.named_filters.find((n) => `n.${n.name}` === k);
      const choice = f?.choices.find((c) => c.name === params.get(k));
      if (f) out.push({ id: k, label: `${f.title}: ${choice?.title ?? params.get(k)}`, drop: (p) => p.delete(k) });
      continue;
    }
    if (!k.startsWith("f.")) continue;
    const [name = "", op = ""] = k.slice(2).split(":");
    const f = meta.filters.find((x) => x.name === name);
    if (!f) continue;
    const values = params.getAll(k);
    if (op === "empty" || op === "notempty") {
      out.push({ id: k, label: `${f.title} ${op === "empty" ? "is empty" : "is not empty"}`, drop: (p) => p.delete(k) });
    } else if (f.kind === "relation" && op === "") {
      for (const v of values) {
        out.push({ id: `${k}=${v}`, label: `${f.title}: ${safeDecode(v)}`, relation: { target: f.target ?? "", value: v }, drop: (p) => withoutValue(p, k, v) });
      }
    } else {
      out.push({ id: k, label: `${f.title} ${opText(f, op, values, zone)}`, drop: (p) => p.delete(k) });
    }
  }
  return out;
}

function withoutValue(p: Params, name: string, value: string) {
  const rest = p.getAll(name).filter((v) => v !== value);
  p.delete(name);
  for (const v of rest) p.append(name, v);
}

function opText(f: api.Filter, op: string, values: string[], zone: Zone): string {
  const day = (v: string) => (f.kind === "datetime" ? dayOf(new Date(v), zone) : v);
  const one = values[0] ?? "";
  if (op === "gte") return f.kind === "date" || f.kind === "datetime" ? `from ${day(one)}` : `≥ ${one}`;
  if (op === "lte") return f.kind === "date" ? `to ${one}` : `≤ ${one}`;
  if (op === "lt") return f.kind === "datetime" ? `to ${plusDays(day(one), -1)}` : `< ${one}`;
  if (op === "gt") return `> ${one}`;
  return `${op ? `${op} ` : "is "}${values.join(", ")}`;
}

/** One chip for each active filter, each with a button that removes it, and a button that removes all. */
export function FilterChips({ meta, params, change, zone }: { meta: api.Resource; params: Params; change: Change; zone: Zone }) {
  const chips = activeChips(meta, params, zone);
  if (chips.length === 0) return null;
  return (
    <ul aria-label="Active filters" className="m-0 mb-3 flex list-none flex-wrap items-center gap-2 p-0">
      {chips.map((c) => (
        <li key={c.id}>
          <Badge variant="secondary" className="gap-1 py-1 pr-1 text-sm">
            {c.relation ? <RecordName target={c.relation.target} value={c.relation.value} fallback={c.label} /> : c.label}
            <button type="button" aria-label={`Remove filter ${c.label}`} className="rounded-full p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => change(c.drop)}>
              <X className="size-3.5" aria-hidden="true" />
            </button>
          </Badge>
        </li>
      ))}
      {chips.length > 1 && (
        <li>
          <Button variant="ghost" size="sm" onClick={() => change(clearFilters)}>Clear all</Button>
        </li>
      )}
    </ul>
  );
}

/** The name of a record of a resource: the column that its meta names. It falls back to the decoded key. */
function RecordName({ target, value, fallback }: { target: string; value: string; fallback: string }) {
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const d = useQuery({ queryKey: ["detail", target, value], queryFn: () => api.detail(target, value), retry: false });
  const meta = site.data?.resources.find((r) => r.name === target);
  const named = valueOf(d.data, meta?.name_column);
  const title = meta?.title ?? target;
  return <>{named !== undefined && named !== null ? `${title}: ${String(named)}` : fallback}</>;
}

/** A named filter is a select. Its default choice is the URL without the parameter; "All" is the choice `all`. */
function Named({ filter, params, change }: { filter: api.NamedFilter; params: Params; change: Change }) {
  const name = `n.${filter.name}`;
  const value = params.get(name) ?? filter.default ?? "all";
  return (
    <label className="grid content-start gap-1 text-xs font-medium text-muted-foreground">
      {filter.title}
      <Select
        value={value}
        onChange={(e) =>
          change((p) => {
            if (e.target.value === (filter.default ?? "all")) p.delete(name);
            else p.set(name, e.target.value);
          })
        }
      >
        <option value="all">All</option>
        {filter.choices.map((c) => (
          <option key={c.name} value={c.name}>{c.title}</option>
        ))}
      </Select>
    </label>
  );
}

function Control({ filter, params, change, zone }: { filter: api.Filter; params: Params; change: Change; zone: Zone }) {
  const nullable = filter.operators.includes("empty");
  const body = () => {
    switch (filter.kind) {
      case "enum":
        return <Choices filter={filter} params={params} change={change} />;
      case "bool":
        return <Bool filter={filter} params={params} change={change} />;
      case "relation":
        return <Relation filter={filter} params={params} change={change} />;
      case "date":
      case "datetime":
        return <Dates filter={filter} params={params} change={change} zone={zone} />;
      case "number":
        return <Range filter={filter} params={params} change={change} />;
      default:
        return <Text filter={filter} params={params} change={change} />;
    }
  };
  return (
    <fieldset className="m-0 grid min-w-0 content-start gap-2 border-0 p-0">
      <legend className={legend}>{filter.title}</legend>
      {body()}
      {nullable && <Empty filter={filter} params={params} change={change} />}
    </fieldset>
  );
}

/** "Is empty" and "is not empty": at most one of the two is on. */
function Empty({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const on = params.has(key(filter, "empty")) ? "empty" : params.has(key(filter, "notempty")) ? "notempty" : "";
  return (
    <Select
      aria-label={`${filter.title}: empty`}
      value={on}
      onChange={(e) =>
        change((p) => {
          p.delete(key(filter, "empty"));
          p.delete(key(filter, "notempty"));
          if (e.target.value) p.set(key(filter, e.target.value), "1");
        })
      }
    >
      <option value="">Empty or not</option>
      <option value="empty">Is empty</option>
      <option value="notempty">Is not empty</option>
    </Select>
  );
}

function Choices({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const chosen = params.getAll(key(filter));
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1">
      {(filter.choices ?? []).map((c) => (
        <label key={c} className="inline-flex items-center gap-1.5 text-sm">
          <input
            type="checkbox"
            checked={chosen.includes(c)}
            onChange={(e) =>
              change((p) => {
                const rest = p.getAll(key(filter)).filter((v) => v !== c);
                p.delete(key(filter));
                for (const v of e.target.checked ? [...rest, c] : rest) p.append(key(filter), v);
              })
            }
          />
          {c}
        </label>
      ))}
    </div>
  );
}

function Bool({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  return (
    <Select
      aria-label={filter.title}
      value={params.get(key(filter)) ?? ""}
      onChange={(e) =>
        change((p) => {
          if (e.target.value) p.set(key(filter), e.target.value);
          else p.delete(key(filter));
        })
      }
    >
      <option value="">Any</option>
      <option value="true">Yes</option>
      <option value="false">No</option>
    </Select>
  );
}

/** A value box that writes on Enter or on leaving the box, not on each key: each write is a request. */
function Box({ label, value, type = "text", narrow = false, onValue }: { label: string; value: string; type?: string; narrow?: boolean; onValue: (v: string) => void }) {
  const [draft, setDraft] = useState(value);
  const [seen, setSeen] = useState(value);
  if (value !== seen) {
    setSeen(value);
    setDraft(value);
  }
  const commit = () => draft !== value && onValue(draft.trim());
  return (
    <Input
      type={type}
      className={narrow ? small : undefined}
      aria-label={label}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => e.key === "Enter" && commit()}
    />
  );
}

const setOrDrop = (p: Params, name: string, value: string) => (value ? p.set(name, value) : p.delete(name));

/** Several values: the box takes a comma list, and the parameter repeats. */
function Text({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  return (
    <Box
      label={filter.title}
      value={params.getAll(key(filter)).join(", ")}
      onValue={(v) =>
        change((p) => {
          p.delete(key(filter));
          for (const part of v.split(",")) if (part.trim()) p.append(key(filter), part.trim());
        })
      }
    />
  );
}

function Range({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const bounds: [string, string][] = [["gte", "From"], ["lte", "To"]];
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Text filter={filter} params={params} change={change} />
      {bounds.map(([op, label]) => (
        <Box key={op} narrow type="number" label={`${filter.title} ${label.toLowerCase()}`} value={params.get(key(filter, op)) ?? ""} onValue={(v) => change((p) => setOrDrop(p, key(filter, op), v))} />
      ))}
    </div>
  );
}

/** The value of the column that names a record, from its detail. */
const valueOf = (detail: api.Detail | undefined, name: string | null | undefined) => {
  const at = detail && name ? detail.fields.findIndex((c) => c.label === name) : -1;
  return at < 0 ? undefined : detail?.values[at];
};

const pad = (n: number) => String(n).padStart(2, "0");
const dayOf = (d: Date, zone: Zone) =>
  zone === "utc" ? `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}` : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;

/** The instant at which a calendar day starts in the zone, as RFC 3339 in UTC. */
function startOf(day: string, zone: Zone): string {
  const [y, m, d] = day.split("-").map(Number) as [number, number, number];
  const at = zone === "utc" ? new Date(Date.UTC(y, m - 1, d)) : new Date(y, m - 1, d);
  return at.toISOString().replace(/\.\d{3}Z$/, "Z");
}

const plusDays = (day: string, n: number) => {
  const [y, m, d] = day.split("-").map(Number) as [number, number, number];
  return dayOf(new Date(Date.UTC(y, m - 1, d + n)), "utc");
};

/** The presets as `[from, to)` calendar days in the zone. `to` is the day after the last day. */
export function presets(now: Date, zone: Zone): { name: string; from: string; to: string }[] {
  const today = dayOf(now, zone);
  return [
    { name: "Today", from: today, to: plusDays(today, 1) },
    { name: "Last 7 days", from: plusDays(today, -6), to: plusDays(today, 1) },
    { name: "Last 30 days", from: plusDays(today, -29), to: plusDays(today, 1) },
    { name: "This month", from: `${today.slice(0, 8)}01`, to: plusDays(today, 1) },
  ];
}

/** A date is `YYYY-MM-DD`. A datetime range is `gte` the start of the first day and `lt` the start of the day after the last. */
function Dates({ filter, params, change, zone }: { filter: api.Filter; params: Params; change: Change; zone: Zone }) {
  const date = filter.kind === "date";
  const lo = date ? key(filter, "gte") : key(filter, "gte");
  const hi = date ? key(filter, "lte") : key(filter, "lt");
  const asDay = (v: string | null) => (v ? (date ? v : dayOf(new Date(v), zone)) : "");
  const from = asDay(params.get(lo));
  // A datetime `lt` is the start of the day after the last day: show the last day.
  const to = date ? (params.get(hi) ?? "") : params.get(hi) ? plusDays(dayOf(new Date(params.get(hi) as string), zone), -1) : "";
  const set = (f: string, t: string) =>
    change((p) => {
      p.delete(lo);
      p.delete(hi);
      if (f) p.set(lo, date ? f : startOf(f, zone));
      if (t) p.set(hi, date ? t : startOf(plusDays(t, 1), zone));
    });
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Input type="date" className={small} aria-label={`${filter.title} from`} value={from} onChange={(e) => set(e.target.value, to)} />
      <Input type="date" className={small} aria-label={`${filter.title} to`} value={to} onChange={(e) => set(from, e.target.value)} />
      <span className="flex flex-wrap gap-1">
        {presets(new Date(), zone).map((x) => (
          <Button key={x.name} variant="plain" size="sm" onClick={() => set(x.from, plusDays(x.to, -1))}>{x.name}</Button>
        ))}
      </span>
    </div>
  );
}

/** A relation filter: the chosen keys as chips with the record's name, and a search box over the target resource with a keyset "more". */
function Relation({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const target = filter.target ?? "";
  const [text, setText] = useState("");
  // The options load when the list opens, not with the page.
  const [open, setOpen] = useState(false);
  const chosen = params.getAll(key(filter));
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const meta = site.data?.resources.find((r) => r.name === target);
  const options = useInfiniteQuery({
    queryKey: ["relation", target, text],
    queryFn: ({ pageParam }) => {
      const p = new URLSearchParams();
      if (text) p.set("q", text);
      if (pageParam) p.set("cursor", pageParam);
      p.set("limit", "10");
      return api.page(target, `?${p}`);
    },
    initialPageParam: "",
    getNextPageParam: (last) => last.next ?? undefined,
    enabled: !!meta && open,
  });
  const add = (k: string) => change((p) => p.append(key(filter), k));
  const remove = (k: string) =>
    change((p) => {
      const rest = p.getAll(key(filter)).filter((v) => v !== k);
      p.delete(key(filter));
      for (const v of rest) p.append(key(filter), v);
    });
  const rows =
    options.data?.pages.flatMap((pg) => {
      const at = pg.columns.findIndex((c) => c.label === meta?.name_column);
      return pg.rows.map((cells, i) => ({ key: pg.keys[i] ?? "", name: String((at < 0 ? null : cells[at]) ?? pg.keys[i] ?? "") }));
    }) ?? [];
  const searchable = !!meta && (meta.search.fields.length > 0 || meta.search.custom);
  return (
    <div className="grid gap-2">
      <span className="chips">
        {chosen.map((k) => (
          <Chip key={k} target={target} value={k} onRemove={() => remove(k)} />
        ))}
      </span>
      {meta ? (
        <Collapsible open={open} onOpenChange={setOpen} title={`Choose ${meta.title}`} titleClassName="text-sm">
          {searchable && <Input type="search" className="mt-2" aria-label={`Search ${meta.title}`} value={text} onChange={(e) => setText(e.target.value)} />}
          <ul className="m-0 my-1 max-h-40 list-none overflow-auto p-0">
            {rows.filter((r) => !chosen.includes(r.key)).map((r) => (
              <li key={r.key}><button type="button" className="w-full rounded-sm px-2 py-1 text-left text-sm hover:bg-accent" onClick={() => add(r.key)}>{r.name}</button></li>
            ))}
          </ul>
          {options.hasNextPage && (
            <Button variant="plain" disabled={options.isFetchingNextPage} onClick={() => options.fetchNextPage()}>More</Button>
          )}
        </Collapsible>
      ) : (
        // The target is no resource of this site: its key is the only thing to type.
        <Box label={`${filter.title} key`} value="" onValue={(v) => v && add(encodeURIComponent(v))} />
      )}
    </div>
  );
}

/** One chosen key: it shows the name of the record when the target is a resource that this user can read. */
function Chip({ target, value, onRemove }: { target: string; value: string; onRemove: () => void }) {
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const d = useQuery({ queryKey: ["detail", target, value], queryFn: () => api.detail(target, value), retry: false });
  const named = valueOf(d.data, site.data?.resources.find((r) => r.name === target)?.name_column);
  const text = named !== undefined && named !== null ? String(named) : safeDecode(value);
  return (
    <span className="chip">
      {text}
      <button type="button" aria-label={`Remove ${text}`} onClick={onRemove}>×</button>
    </span>
  );
}
