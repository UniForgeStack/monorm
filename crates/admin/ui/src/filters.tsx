import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import * as api from "./api";
import { type Zone } from "./format";
import { Button } from "./ui";

/** The keys of a list URL that hold a position, not a view: any change of the view drops them. */
export const POSITION = ["cursor", "before", "last", "page"];

type Params = URLSearchParams;
type Change = (edit: (p: Params) => void) => void;

const key = (f: api.Filter, op = "") => `f.${f.name}${op ? `:${op}` : ""}`;

/** The filter bar of a list: named filters first, then one control per filter. Every control writes to the URL parameters. */
export function FilterBar({ meta, params, change, zone }: { meta: api.Resource; params: Params; change: Change; zone: Zone }) {
  if (meta.filters.length === 0 && meta.named_filters.length === 0) return null;
  const active = [...params.keys()].some((k) => k.startsWith("f.") || k.startsWith("n."));
  return (
    <div className="filters" role="group" aria-label="Filters">
      {meta.named_filters.map((n) => (
        <Named key={n.name} filter={n} params={params} change={change} />
      ))}
      {meta.filters.map((f) => (
        <Control key={f.name} filter={f} params={params} change={change} zone={zone} />
      ))}
      {active && (
        <Button
          variant="plain"
          onClick={() =>
            change((p) => {
              for (const k of [...p.keys()]) if (k.startsWith("f.") || k.startsWith("n.")) p.delete(k);
            })
          }
        >
          Clear filters
        </Button>
      )}
    </div>
  );
}

/** A named filter is a select. Its default choice is the URL without the parameter; "All" is the choice `all`. */
function Named({ filter, params, change }: { filter: api.NamedFilter; params: Params; change: Change }) {
  const name = `n.${filter.name}`;
  const value = params.get(name) ?? filter.default ?? "all";
  return (
    <label className="filter">
      {filter.title}
      <select
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
      </select>
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
    <fieldset className="filter">
      <legend>{filter.title}</legend>
      {body()}
      {nullable && <Empty filter={filter} params={params} change={change} />}
    </fieldset>
  );
}

/** "Is empty" and "is not empty": at most one of the two is on. */
function Empty({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const on = params.has(key(filter, "empty")) ? "empty" : params.has(key(filter, "notempty")) ? "notempty" : "";
  return (
    <select
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
    </select>
  );
}

function Choices({ filter, params, change }: { filter: api.Filter; params: Params; change: Change }) {
  const chosen = params.getAll(key(filter));
  return (
    <div className="choices">
      {(filter.choices ?? []).map((c) => (
        <label key={c}>
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
    <select
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
    </select>
  );
}

/** A value box that writes on Enter or on leaving the box, not on each key: each write is a request. */
function Box({ label, value, type = "text", onValue }: { label: string; value: string; type?: string; onValue: (v: string) => void }) {
  const [draft, setDraft] = useState(value);
  const [seen, setSeen] = useState(value);
  if (value !== seen) {
    setSeen(value);
    setDraft(value);
  }
  const commit = () => draft !== value && onValue(draft.trim());
  return (
    <input
      type={type}
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
    <div className="range">
      <Text filter={filter} params={params} change={change} />
      {bounds.map(([op, label]) => (
        <Box key={op} type="number" label={`${filter.title} ${label.toLowerCase()}`} value={params.get(key(filter, op)) ?? ""} onValue={(v) => change((p) => setOrDrop(p, key(filter, op), v))} />
      ))}
    </div>
  );
}

/** The column that names a record: the first one that is not part of its key (an id says little), or the first. */
const nameAt = (columns: api.Column[], key: string[]) => Math.max(0, columns.findIndex((c) => !key.includes(c.label)));

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
    <div className="range">
      <input type="date" aria-label={`${filter.title} from`} value={from} onChange={(e) => set(e.target.value, to)} />
      <input type="date" aria-label={`${filter.title} to`} value={to} onChange={(e) => set(from, e.target.value)} />
      <span className="presets">
        {presets(new Date(), zone).map((x) => (
          <Button key={x.name} variant="plain" onClick={() => set(x.from, plusDays(x.to, -1))}>{x.name}</Button>
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
      const at = nameAt(pg.columns, meta?.key ?? []);
      return pg.rows.map((cells, i) => ({ key: pg.keys[i] ?? "", name: String(cells[at] ?? pg.keys[i] ?? "") }));
    }) ?? [];
  const searchable = !!meta && (meta.search.fields.length > 0 || meta.search.custom);
  return (
    <div className="relation">
      <span className="chips">
        {chosen.map((k) => (
          <Chip key={k} target={target} value={k} onRemove={() => remove(k)} />
        ))}
      </span>
      {meta ? (
        <details onToggle={(e) => setOpen(e.currentTarget.open)}>
          <summary>Pick a {meta.title}</summary>
          {searchable && <input type="search" aria-label={`Search ${meta.title}`} value={text} onChange={(e) => setText(e.target.value)} />}
          <ul className="options">
            {rows.filter((r) => !chosen.includes(r.key)).map((r) => (
              <li key={r.key}><button type="button" onClick={() => add(r.key)}>{r.name}</button></li>
            ))}
          </ul>
          {options.hasNextPage && (
            <Button variant="plain" disabled={options.isFetchingNextPage} onClick={() => options.fetchNextPage()}>More</Button>
          )}
        </details>
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
  const at = d.data ? nameAt(d.data.fields, site.data?.resources.find((r) => r.name === target)?.key ?? []) : 0;
  const named = d.data?.values[at];
  const text = named !== undefined && named !== null ? String(named) : decodeURIComponent(value);
  return (
    <span className="chip">
      {text}
      <button type="button" aria-label={`Remove ${text}`} onClick={onRemove}>×</button>
    </span>
  );
}
