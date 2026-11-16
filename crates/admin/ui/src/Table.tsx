import { flexRender, getCoreRowModel, useReactTable, type ColumnDef } from "@tanstack/react-table";
import { useMemo, type ReactNode } from "react";
import type { Column } from "./api";
import { cell, type Zone } from "./format";
import { Link, detailUrl } from "./router";

interface Row {
  cells: unknown[];
  key: string;
}

/** One column per meta column. The first one links to the detail of its row. */
export function Table({ resource, columns, rows, keys, zone, sortable = [], unindexed = [], sort = "", onSort, select, rowActions }: { resource: string; columns: Column[]; rows: unknown[][]; keys: string[]; zone: Zone; sortable?: string[]; unindexed?: string[]; sort?: string; onSort?: (label: string) => void; select?: { chosen: Set<string>; toggle: (key: string) => void; toggleAll: () => void }; rowActions?: (index: number, key: string) => ReactNode }) {
  const defs = useMemo<ColumnDef<Row>[]>(
    () =>
      columns.map((c, i) => ({
        id: c.label,
        header: c.title,
        cell: ({ row }) => {
          const text = cell(row.original.cells[i], c, zone);
          const none = c.empty ?? "—";
          if (i === 0) return <Link href={detailUrl(resource, row.original.key)}>{text || none}</Link>;
          return text || <span className="muted">{none}</span>;
        },
      })),
    [columns, resource, zone],
  );
  const data = useMemo<Row[]>(() => rows.map((cells, i) => ({ cells, key: keys[i] ?? "" })), [rows, keys]);
  const table = useReactTable({ data, columns: defs, getCoreRowModel: getCoreRowModel() });
  const all = keys.length > 0 && !!select && keys.every((k) => select.chosen.has(k));
  return (
    <div className="scroll">
      <table className="table">
        <thead>
          {table.getHeaderGroups().map((g) => (
            <tr key={g.id}>
              {select && (
                <th scope="col" className="pick">
                  <input type="checkbox" aria-label="Select all rows of this page" checked={all} onChange={select.toggleAll} />
                </th>
              )}
              {g.headers.map((h) => (
                <th key={h.id} scope="col" aria-sort={ariaSort(sort, h.column.id)}>
                  {onSort && sortable.includes(h.column.id) ? (
                    <button type="button" className="sort" title={unindexed.includes(h.column.id) ? "No index: a large table can refuse this sort" : undefined} onClick={() => onSort(h.column.id)}>
                      {flexRender(h.column.columnDef.header, h.getContext())}
                      <span aria-hidden="true">{ariaSort(sort, h.column.id) === "ascending" ? " ▲" : ariaSort(sort, h.column.id) === "descending" ? " ▼" : ""}</span>
                    </button>
                  ) : (
                    flexRender(h.column.columnDef.header, h.getContext())
                  )}
                </th>
              ))}
              {rowActions && <th scope="col"><span className="sr-only">Actions</span></th>}
            </tr>
          ))}
        </thead>
        <tbody>
          {table.getRowModel().rows.map((r) => (
            <tr key={r.id} aria-selected={select?.chosen.has(r.original.key) || undefined}>
              {select && (
                <td className="pick">
                  <input type="checkbox" aria-label={`Select row ${decodeURIComponent(r.original.key)}`} checked={select.chosen.has(r.original.key)} onChange={() => select.toggle(r.original.key)} />
                </td>
              )}
              {r.getVisibleCells().map((c) => (
                <td key={c.id}>{flexRender(c.column.columnDef.cell, c.getContext())}</td>
              ))}
              {rowActions && <td className="row-actions">{rowActions(r.index, r.original.key)}</td>}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The sort is `-label` for descending and `label` for ascending, on its first column. */
function ariaSort(sort: string, label: string): "ascending" | "descending" | undefined {
  const first = sort.split(",")[0] ?? "";
  if (first === label) return "ascending";
  if (first === `-${label}`) return "descending";
  return undefined;
}
