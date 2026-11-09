import { useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent } from "react";
import * as api from "./api";
import { FilterBar, POSITION } from "./filters";
import { type Zone } from "./format";
import { navigate, rememberList, useSearch } from "./router";
import { Table } from "./Table";
import { Alert, Bypass, Button, message } from "./ui";

const SIZES = [10, 25, 50, 100, 200];

/** The state of a list is the query string of the page: a copy of the URL parameters, and a way to write a new one. */
export function useListState(): { params: URLSearchParams; change: (edit: (p: URLSearchParams) => void, keepPosition?: boolean) => void } {
  const search = useSearch();
  const params = new URLSearchParams(search);
  const change = (edit: (p: URLSearchParams) => void, keepPosition = false) => {
    const next = new URLSearchParams(window.location.search);
    edit(next);
    if (!keepPosition) for (const k of POSITION) next.delete(k);
    const text = next.toString();
    navigate(`${window.location.pathname}${text ? `?${text}` : ""}`);
  };
  return { params, change };
}

/** A list page: the state is in the URL, so a reload, the back button and a link all return to it. */
export function ListPage({ resource, zone, site }: { resource: string; zone: Zone; site: api.Site | undefined }) {
  const { params, change } = useListState();
  const text = params.toString();
  useEffect(() => rememberList(resource, text), [resource, text]);
  const meta = site?.resources.find((r) => r.name === resource);
  return (
    <section>
      <h1>{meta?.title ?? resource}</h1>
      <ListView key={resource} resource={resource} zone={zone} meta={meta} params={params} change={change} />
    </section>
  );
}

/** The rows with their filters, pager, sort headers, search box and page size, and the count after the rows. `scope` makes it the list of one record's to-many relation. */
export function ListView({ resource, zone, meta, params, change, scope }: { resource: string; zone: Zone; meta: api.Resource | undefined; params: URLSearchParams; change: (edit: (p: URLSearchParams) => void, keepPosition?: boolean) => void; scope?: api.Scope }) {
  const q = params.get("q") ?? "";
  const [draft, setDraft] = useState(q);
  const [exact, setExact] = useState(false);
  // A new search or filter has a new count: it is capped again until the user asks.
  const view = [...params].filter(([k]) => k === "q" || k === "without" || k.startsWith("f.") || k.startsWith("n.")).join("&");
  const [seen, setSeen] = useState(view);
  if (view !== seen) {
    setSeen(view);
    setExact(false);
    setDraft(q);
  }
  // The `tab` parameter picks the tab of a detail page: the list request does not carry it.
  const apiParams = new URLSearchParams(params);
  apiParams.delete("tab");
  const search = apiParams.toString() ? `?${apiParams}` : "";
  const scopeKey = scope ? `${scope.resource}/${scope.key}/${scope.rel}` : "";
  const rows = useQuery({ queryKey: ["list", resource, scopeKey, search], queryFn: () => api.page(resource, search, scope), placeholderData: (prev) => prev });
  // The count waits for the rows: it is a second request, and it never delays them.
  // A custom search counts in the rows request (one call of the function), so the second request is skipped.
  const inline = rows.data?.count;
  const counted = useQuery({
    queryKey: ["count", resource, scopeKey, view, exact],
    queryFn: () => api.count(resource, search, exact, scope),
    enabled: rows.isSuccess && inline === undefined,
    placeholderData: (prev) => prev,
    retry: false,
  });
  const total: { data?: api.Count; isError: boolean; error: unknown } =
    inline !== undefined ? { data: { count: inline, capped: rows.data?.capped ?? false }, isError: false, error: null } : counted;
  const sort = params.get("sort") ?? "";
  const set = (name: string, value: string | null) => change((p) => (value ? p.set(name, value) : p.delete(name)));
  const search_ = (e: FormEvent) => {
    e.preventDefault();
    set("q", draft.trim() || null);
  };
  const sortBy = (label: string) => {
    const first = sort.split(",")[0] ?? "";
    set("sort", first === label ? `-${label}` : first === `-${label}` ? null : label);
  };
  if (!rows.data && !rows.isError) return <p className="muted">Loading…</p>;
  const page = rows.data;
  const searchable = !!meta && (meta.search.fields.length > 0 || meta.search.custom);
  const pageSize = meta?.page_size ?? 25;
  const limit = Number(params.get("limit")) || pageSize;
  const sizes = [...new Set([...SIZES, pageSize, limit])].filter((n) => n <= (meta?.max_page_size ?? 200) || n === limit).sort((a, b) => a - b);
  const pages = total.data && !total.data.capped ? Math.max(1, Math.ceil(total.data.count / limit)) : null;
  const position = (name: string, value: string) => change((p) => p.set(name, value));
  const jump = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const n = Number(new FormData(e.currentTarget).get("page"));
    if (pages && Number.isInteger(n) && n >= 1 && n <= pages) position("page", String(n));
  };
  return (
    <>
      <div className="toolbar">
        {searchable && (
          <form role="search" className="search" onSubmit={search_}>
            <input type="search" aria-label="Search" placeholder="Search" value={draft} onChange={(e) => setDraft(e.target.value)} />
            <Button type="submit" variant="plain">Search</Button>
          </form>
        )}
        {meta?.bypass && meta.bypass.length > 0 && <Bypass names={meta.bypass} params={params} change={change} />}
        <label className="size">
          Rows per page
          <select value={limit} onChange={(e) => set("limit", e.target.value)}>
            {sizes.map((n) => (
              <option key={n} value={n}>{n}</option>
            ))}
          </select>
        </label>
      </div>
      {meta && <FilterBar meta={meta} params={params} change={change} zone={zone} />}
      {!page ? (
        // The toolbar stays: a refused sort, search or filter is undone from it.
        <>
          <Alert>{message(rows.error)}</Alert>
          {sort && <Button variant="plain" onClick={() => set("sort", null)}>Clear the sort</Button>}
        </>
      ) : (
        <>
          <Table
            resource={resource}
            columns={page.columns}
            rows={page.rows}
            keys={page.keys}
            zone={zone}
            sortable={meta?.sortable.map((s) => s.column) ?? []}
            unindexed={meta?.sortable.filter((s) => !s.indexed).map((s) => s.column) ?? []}
            sort={sort}
            onSort={sortBy}
          />
          {page.rows.length === 0 && <p className="muted">No records.</p>}
          <div className="pager">
            <Button variant="plain" disabled={!page.prev} onClick={() => change(() => undefined)}>First</Button>
            <Button variant="plain" disabled={!page.prev} onClick={() => page.prev && position("before", page.prev)}>Previous</Button>
            <Button variant="plain" disabled={!page.next} onClick={() => page.next && position("cursor", page.next)}>Next</Button>
            <Button variant="plain" disabled={!page.next} onClick={() => position("last", "1")}>Last</Button>
            {pages && pages > 1 && (
              <form className="jump" onSubmit={jump}>
                <input name="page" type="number" min={1} max={pages} aria-label="Page number" placeholder={`1–${pages}`} />
                <Button type="submit" variant="plain">Go</Button>
              </form>
            )}
            <span className="count" role="status">
              {total.isError ? message(total.error) : total.data ? countText(total.data) : "Counting…"}
              {total.data?.capped && !exact && (
                <Button variant="plain" onClick={() => setExact(true)}>Count exactly</Button>
              )}
            </span>
          </div>
        </>
      )}
    </>
  );
}

function countText({ count, capped }: api.Count): string {
  const n = count.toLocaleString("en-US");
  return `${n}${capped ? "+" : ""} ${count === 1 && !capped ? "record" : "records"}`;
}
