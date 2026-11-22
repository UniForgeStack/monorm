import { useQuery } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight, Columns3, Ellipsis, Inbox, Plus, Rows3, SearchX, X } from "lucide-react";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import * as api from "./api";
import { DateBar } from "./DateBar";
import { FilterChips, FilterPopover, POSITION } from "./filters";
import { type Zone } from "./format";
import { Link, navigate, newUrl, rememberList, useSearch } from "./router";
import { useListEdit } from "./ListEdit";
import { Table, TableSkeleton } from "./Table";
import { ActionDialog, deleteAction, needsDialog, useQuick, type Target } from "./Actions";
import { buttonVariants } from "./components/ui/button";
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "./components/ui/dropdown-menu";
import { Input, Select } from "./components/ui/input";
import { Alert, Bypass, Button, message } from "./ui";

const SIZES = [10, 25, 50, 100, 200];

const DENSITY = "monorm-admin-density";
const HIDDEN = "monorm-admin-columns:";

/** The density of every list (`compact` or `comfortable`) lives in the browser, like the time zone. */
function useDensity(): [boolean, () => void] {
  const [compact, setCompact] = useState(() => localStorage.getItem(DENSITY) === "compact");
  return [
    compact,
    () => {
      localStorage.setItem(DENSITY, compact ? "comfortable" : "compact");
      setCompact(!compact);
    },
  ];
}

/** The labels of the columns that the user hid on one resource: they live in the browser. */
function useHidden(resource: string): [Set<string>, (label: string, on: boolean) => void] {
  const [hidden, setHidden] = useState<Set<string>>(() => {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem(HIDDEN + resource) ?? "[]");
      return new Set(Array.isArray(saved) ? saved.filter((x): x is string => typeof x === "string") : []);
    } catch {
      return new Set();
    }
  });
  return [
    hidden,
    (label, on) => {
      const next = new Set(hidden);
      if (on) next.add(label);
      else next.delete(label);
      localStorage.setItem(HIDDEN + resource, JSON.stringify([...next]));
      setHidden(next);
    },
  ];
}

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
  // Actions: the selection lives in the state of the list and ends with the page, the search or the filters.
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [allMatching, setAllMatching] = useState(false);
  const [pending, setPending] = useState<{ action: api.ActionInfo; target: Target } | null>(null);
  const [pageSeen, setPageSeen] = useState(search);
  if (pageSeen !== search) {
    setPageSeen(search);
    setChosen(new Set());
    setAllMatching(false);
    setPending(null);
  }
  // Only a run on the selection ends it: a row action leaves the selection alone.
  const clearSelection = (target: Target) => {
    if (!target.ofSelection) return;
    setChosen(new Set());
    setAllMatching(false);
  };
  const quick = useQuick(resource, (action, target) => setPending({ action, target }), clearSelection);
  const [compact, toggleDensity] = useDensity();
  const [hidden, setHiddenColumn] = useHidden(resource);
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const targets = useMemo(() => new Set(site.data?.resources.map((r) => r.name) ?? []), [site.data]);
  const sort = params.get("sort") ?? "";
  const set = (name: string, value: string | null) => change((p) => (value ? p.set(name, value) : p.delete(name)));
  const onSearch = (e: FormEvent) => {
    e.preventDefault();
    set("q", draft.trim() || null);
  };
  const sortBy = (label: string) => {
    const first = sort.split(",")[0] ?? "";
    set("sort", first === label ? `-${label}` : first === `-${label}` ? null : label);
  };
  const page = rows.data;
  // Only the main list edits in its cells: a tab shows the rows of a relation.
  const listEdit = useListEdit({ resource, meta, page, zone, enabled: !scope });
  const searchable = !!meta && (meta.search.fields.length > 0 || meta.search.custom);
  const pageSize = meta?.page_size ?? 25;
  const limit = Number(params.get("limit")) || pageSize;
  const sizes = [...new Set([...SIZES, pageSize, limit])].filter((n) => n <= (meta?.max_page_size ?? 200) || n === limit).sort((a, b) => a - b);
  const pages = total.data && !total.data.capped ? Math.max(1, Math.ceil(total.data.count / limit)) : null;
  const position = (name: string, value: string) => change((p) => p.set(name, value));
  const actions = meta?.actions ?? [];
  const bulkActions = [...actions.filter((a) => a.placement === "bulk"), ...(meta && meta.writable !== false && meta.bulk_delete ? [deleteAction(meta)] : [])];
  const rowActions = actions.filter((a) => a.placement === "row");
  // A list action runs on the whole list: a tab has no such action.
  const listActions = scope ? [] : actions.filter((a) => a.placement === "list");
  const filters = new URLSearchParams([...params].filter(([k]) => k === "q" || k === "without" || k.startsWith("f.") || k.startsWith("n.")));
  const filterSearch = filters.toString() ? `?${filters}` : "";
  const matching = total.data ? `${total.data.count.toLocaleString("en-US")}${total.data.capped ? "+" : ""}` : "";
  // Unsaved edits end with a bulk action as with a page change: the user confirms first.
  const go = (action: api.ActionInfo, target: Target) => {
    if (!listEdit.ask()) return;
    if (needsDialog(action)) setPending({ action, target });
    else void quick(action, target);
  };
  const selected: Target = allMatching
    ? { all: true, ofSelection: true, search: filterSearch, scope, what: `all ${matching} matching records`.replace("all  ", "all ") }
    : { keys: [...chosen], ofSelection: true, scope, what: `${chosen.size} ${chosen.size === 1 ? "record" : "records"}` };
  const onPage = page?.keys ?? [];
  const pageChosen = onPage.length > 0 && onPage.every((k) => chosen.has(k));
  const moreMatching = !!total.data && total.data.count > onPage.length;
  const select =
    bulkActions.length > 0
      ? {
          chosen: allMatching ? new Set(onPage) : chosen,
          toggle: (k: string) => {
            setAllMatching(false);
            setChosen((s) => {
              const next = new Set(s);
              if (!next.delete(k)) next.add(k);
              return next;
            });
          },
          toggleAll: () => {
            setAllMatching(false);
            setChosen(pageChosen ? new Set() : new Set(onPage));
          },
        }
      : undefined;
  const jump = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const n = Number(new FormData(e.currentTarget).get("page"));
    if (pages && Number.isInteger(n) && n >= 1 && n <= pages) position("page", String(n));
  };
  const filtered = filters.size > 0;
  const clearView = () =>
    change((p) => {
      p.delete("q");
      for (const k of [...p.keys()]) if (k.startsWith("f.") || k.startsWith("n.")) p.delete(k);
    });
  return (
    <>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {searchable && (
          <form role="search" className="flex items-center gap-2" onSubmit={onSearch}>
            <Input type="search" className="w-56" aria-label="Search" placeholder="Search" value={draft} onChange={(e) => setDraft(e.target.value)} />
            <Button type="submit" variant="plain">Search</Button>
          </form>
        )}
        {meta && <FilterPopover meta={meta} params={params} change={change} zone={zone} />}
        {meta?.bypass && meta.bypass.length > 0 && <Bypass names={meta.bypass} params={params} change={change} />}
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {page && <ColumnsMenu columns={page.columns} hidden={hidden} onChange={setHiddenColumn} />}
          <Button variant="plain" aria-pressed={compact} title={compact ? "Switch to comfortable rows" : "Switch to compact rows"} onClick={toggleDensity}>
            <Rows3 aria-hidden="true" />
            Compact
          </Button>
          {listActions.map((a) => (
            <Button key={a.name} variant="plain" onClick={() => go(a, { search: filterSearch, what: "all the records that match the filters" })}>{a.label}</Button>
          ))}
          {!scope && api.may(meta, "create") && (
            <Link className={buttonVariants()} href={newUrl(resource)}>
              <Plus aria-hidden="true" />
              Add
            </Link>
          )}
        </div>
      </div>
      {meta && <FilterChips meta={meta} params={params} change={change} zone={zone} />}
      {meta && !scope && <DateBar resource={resource} meta={meta} params={params} change={change} zone={zone} />}
      {!page && rows.isError ? (
        // The toolbar stays: a refused sort, search or filter is undone from it.
        <div className="grid gap-3">
          <Alert>{message(rows.error)}</Alert>
          <div className="flex flex-wrap gap-2">
            <Button variant="plain" onClick={() => void rows.refetch()}>Try again</Button>
            {sort && <Button variant="plain" onClick={() => set("sort", null)}>Clear the sort</Button>}
          </div>
        </div>
      ) : !page ? (
        <TableSkeleton columns={meta?.columns.length ?? 4} compact={compact} />
      ) : (
        <>
          {select && (chosen.size > 0 || allMatching) && (
            <div className="mb-3 flex flex-wrap items-center gap-2 rounded-lg border border-border bg-accent px-3 py-2 text-sm" role="group" aria-label="Bulk actions">
              <span role="status" className="mr-2 font-medium">{allMatching ? `All ${matching} matching records selected` : `${chosen.size} selected`}</span>
              {!allMatching && pageChosen && moreMatching && <Button variant="plain" size="sm" onClick={() => setAllMatching(true)}>Select all {matching} matching records</Button>}
              {bulkActions.map((a) => (
                <Button key={a.name} variant={a.name === "bulk_delete" ? "destructive" : "plain"} size="sm" onClick={() => go(a, selected)}>{a.name === "bulk_delete" ? "Delete selected" : a.label}</Button>
              ))}
              <Button variant="ghost" size="sm" className="ml-auto" onClick={() => { setAllMatching(false); setChosen(new Set()); }}>
                <X aria-hidden="true" />
                Clear the selection
              </Button>
            </div>
          )}
          {pending && <ActionDialog key={pending.action.name} resource={resource} action={pending.action} target={pending.target} onClose={() => setPending(null)} onDone={clearSelection} />}
          {page.rows.length === 0 ? (
            <Empty searching={filtered || !!q} canAdd={!scope && api.may(meta, "create")} resource={resource} title={meta?.title ?? resource} onClear={clearView} />
          ) : (
            <Table
              select={select}
              rowActions={rowActions.length > 0 ? (i, key) => <RowMenu actions={rowActions} flags={page.flags?.[i] ?? {}} may={!(meta?.can ?? []).includes("change") || (page.flags?.[i] ?? {}).change === true} label={decodeURIComponent(key)} onRun={(a) => go(a, { keys: [key], scope, what: "this record" })} /> : undefined}
              resource={resource}
              columns={page.columns}
              rows={page.rows}
              keys={page.keys}
              labels={page.labels}
              targets={targets}
              hidden={hidden}
              compact={compact}
              busy={rows.isPlaceholderData}
              zone={zone}
              sortable={meta?.sortable.map((s) => s.column) ?? []}
              unindexed={meta?.sortable.filter((s) => !s.indexed).map((s) => s.column) ?? []}
              sort={sort}
              onSort={sortBy}
              edit={listEdit.edit}
            />
          )}
          {listEdit.bar}
          <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
            <span className="flex items-center gap-2 text-muted-foreground" role="status">
              {total.isError ? message(total.error) : total.data ? countText(total.data) : "Counting…"}
              {total.data?.capped && !exact && (
                <Button variant="plain" size="sm" onClick={() => setExact(true)}>Count exactly</Button>
              )}
            </span>
            <div className="ml-auto flex flex-wrap items-center gap-3">
              <label className="flex items-center gap-2 text-muted-foreground">
                Rows per page
                <Select className="h-8 w-auto py-0" value={limit} onChange={(e) => set("limit", e.target.value)}>
                  {sizes.map((n) => (
                    <option key={n} value={n}>{n}</option>
                  ))}
                </Select>
              </label>
              {pages && pages > 1 && (
                <form className="flex items-center gap-1" onSubmit={jump}>
                  <Input name="page" type="number" min={1} max={pages} className="h-8 w-24" aria-label="Page number" placeholder={`1–${pages}`} />
                  <Button type="submit" variant="plain" size="sm">Go</Button>
                </form>
              )}
              <nav aria-label="Pagination" className="flex items-center gap-1">
                <Button variant="plain" size="icon" className="size-8" aria-label="First" disabled={!page.prev} onClick={() => change(() => undefined)}>
                  <ChevronsLeft aria-hidden="true" />
                </Button>
                <Button variant="plain" size="sm" disabled={!page.prev} onClick={() => page.prev && position("before", page.prev)}>
                  <ChevronLeft aria-hidden="true" />
                  Previous
                </Button>
                <Button variant="plain" size="sm" disabled={!page.next} onClick={() => page.next && position("cursor", page.next)}>
                  Next
                  <ChevronRight aria-hidden="true" />
                </Button>
                <Button variant="plain" size="icon" className="size-8" aria-label="Last" disabled={!page.next} onClick={() => position("last", "1")}>
                  <ChevronsRight aria-hidden="true" />
                </Button>
              </nav>
            </div>
          </div>
        </>
      )}
    </>
  );
}

/** The choice of the columns to show: the first one stays, it links to the record. */
function ColumnsMenu({ columns, hidden, onChange }: { columns: api.Column[]; hidden: Set<string>; onChange: (label: string, on: boolean) => void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="plain">
          <Columns3 aria-hidden="true" />
          Columns
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>Show columns</DropdownMenuLabel>
        {columns.map((c, i) => (
          <DropdownMenuCheckboxItem key={c.label} checked={!hidden.has(c.label)} disabled={i === 0} onSelect={(e) => e.preventDefault()} onCheckedChange={(on) => onChange(c.label, !on)}>
            {c.title}
          </DropdownMenuCheckboxItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** The actions of one row in a menu. An action that does not fit stays in the menu, off, with the reason. */
function RowMenu({ actions, flags, may, label, onRun }: { actions: api.ActionInfo[]; flags: Record<string, unknown>; may: boolean; label: string; onRun: (a: api.ActionInfo) => void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="size-8" aria-label={`Actions for row ${label}`}>
          <Ellipsis aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {actions.map((a) => {
          const fits = !a.applies || flags[`applies:${a.name}`] === true;
          return (
            <DropdownMenuItem key={a.name} disabled={!may || !fits} title={!may ? "This record does not allow change" : !fits ? "This action does not apply to this record" : undefined} onSelect={() => onRun(a)}>
              {a.label}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** No row: a view that matches nothing offers to clear itself; an empty resource offers to add a record. */
function Empty({ searching, canAdd, resource, title, onClear }: { searching: boolean; canAdd: boolean; resource: string; title: string; onClear: () => void }) {
  const Icon = searching ? SearchX : Inbox;
  return (
    <div className="grid justify-items-center gap-2 rounded-lg border border-dashed border-border bg-card px-6 py-14 text-center">
      <Icon className="size-8 text-muted-foreground" aria-hidden="true" />
      <p className="m-0 font-medium">{searching ? "No records match" : "No records yet"}</p>
      <p className="m-0 text-sm text-muted-foreground">{searching ? "Change the search or the filters to see more records." : `There is no ${title} record to show.`}</p>
      {searching ? (
        <Button variant="plain" onClick={onClear}>Clear the search and filters</Button>
      ) : (
        canAdd && (
          <Link className={buttonVariants()} href={newUrl(resource)}>
            <Plus aria-hidden="true" />
            Add
          </Link>
        )
      )}
    </div>
  );
}

function countText({ count, capped }: api.Count): string {
  const n = count.toLocaleString("en-US");
  return `${n}${capped ? "+" : ""} ${count === 1 && !capped ? "record" : "records"}`;
}
