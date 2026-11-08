import { useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent } from "react";
import * as api from "./api";
import { type Zone } from "./format";
import { Table } from "./Table";
import { Alert, Button, message } from "./ui";

const SIZES = [10, 25, 50, 100, 200];

/** A list: the rows with their pager, sort headers, search box and page size, and the count after the rows. */
export function ListPage({ resource, zone, site }: { resource: string; zone: Zone; site: api.Site | undefined }) {
  const [params, setParams] = useState<api.ListParams>({});
  const [draft, setDraft] = useState("");
  const [exact, setExact] = useState(false);
  useEffect(() => {
    setParams({});
    setDraft("");
    setExact(false);
  }, [resource]);
  const meta = site?.resources.find((r) => r.name === resource);
  const q = params.q ?? "";
  const rows = useQuery({ queryKey: ["list", resource, params], queryFn: () => api.page(resource, params), placeholderData: (prev) => prev });
  // The count waits for the rows: it is a second request, and it never delays them.
  const total = useQuery({
    queryKey: ["count", resource, q, exact],
    queryFn: () => api.count(resource, q, exact),
    enabled: rows.isSuccess,
    placeholderData: (prev) => prev,
    retry: false,
  });
  const restart = (change: api.ListParams) => setParams((p) => ({ sort: p.sort, q: p.q, limit: p.limit, ...change }));
  const search = (e: FormEvent) => {
    e.preventDefault();
    setExact(false);
    setParams((p) => ({ sort: p.sort, limit: p.limit, q: draft.trim() || undefined }));
  };
  const sortBy = (label: string) => {
    const first = (params.sort ?? "").split(",")[0] ?? "";
    const next = first === label ? `-${label}` : first === `-${label}` ? undefined : label;
    setParams((p) => ({ q: p.q, limit: p.limit, sort: next }));
  };
  if (rows.isError) return <Alert>{message(rows.error)}</Alert>;
  if (!rows.data) return <p className="muted">Loading…</p>;
  const page = rows.data;
  const searchable = !!meta && (meta.search.fields.length > 0 || meta.search.custom);
  const limit = params.limit ?? meta?.page_size ?? 50;
  const sizes = [...new Set([...SIZES, meta?.page_size ?? 50])].filter((n) => n <= (meta?.max_page_size ?? 200)).sort((a, b) => a - b);
  const pages = total.data && !total.data.capped ? Math.max(1, Math.ceil(total.data.count / limit)) : null;
  const jump = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const n = Number(new FormData(e.currentTarget).get("page"));
    if (pages && Number.isInteger(n) && n >= 1 && n <= pages) restart({ page: n });
  };
  return (
    <section>
      <h1>{meta?.title ?? resource}</h1>
      <div className="toolbar">
        {searchable && (
          <form role="search" className="search" onSubmit={search}>
            <input type="search" aria-label="Search" placeholder="Search" value={draft} onChange={(e) => setDraft(e.target.value)} />
            <Button type="submit" variant="plain">Search</Button>
          </form>
        )}
        <label className="size">
          Rows per page
          <select value={limit} onChange={(e) => restart({ limit: Number(e.target.value) })}>
            {sizes.map((n) => (
              <option key={n} value={n}>{n}</option>
            ))}
          </select>
        </label>
      </div>
      <Table
        resource={resource}
        columns={page.columns}
        rows={page.rows}
        keys={page.keys}
        zone={zone}
        sortable={meta?.sortable.map((s) => s.column) ?? []}
        sort={params.sort ?? ""}
        onSort={sortBy}
      />
      {page.rows.length === 0 && <p className="muted">No records.</p>}
      <div className="pager">
        <Button variant="plain" disabled={!page.prev} onClick={() => restart({})}>First</Button>
        <Button variant="plain" disabled={!page.prev} onClick={() => page.prev && restart({ before: page.prev })}>Previous</Button>
        <Button variant="plain" disabled={!page.next} onClick={() => page.next && restart({ cursor: page.next })}>Next</Button>
        <Button variant="plain" disabled={!page.next} onClick={() => restart({ last: true })}>Last</Button>
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
    </section>
  );
}

function countText({ count, capped }: api.Count): string {
  const n = count.toLocaleString("en-US");
  return `${n}${capped ? "+" : ""} ${count === 1 && !capped ? "record" : "records"}`;
}
