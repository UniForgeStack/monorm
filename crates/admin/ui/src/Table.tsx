import { flexRender, getCoreRowModel, useReactTable, type ColumnDef } from "@tanstack/react-table";
import { useMemo } from "react";
import type { Column } from "./api";
import { cell, type Zone } from "./format";
import { Link, detailUrl } from "./router";

interface Row {
  cells: unknown[];
  key: string;
}

/** One column per meta column. The first one links to the detail of its row. */
export function Table({ resource, columns, rows, keys, zone, sortable = [], sort = "", onSort }: { resource: string; columns: Column[]; rows: unknown[][]; keys: string[]; zone: Zone; sortable?: string[]; sort?: string; onSort?: (label: string) => void }) {
  const defs = useMemo<ColumnDef<Row>[]>(
    () =>
      columns.map((c, i) => ({
        id: c.label,
        header: c.title,
        cell: ({ row }) => {
          const text = cell(row.original.cells[i], c, zone);
          if (i === 0) return <Link href={detailUrl(resource, row.original.key)}>{text || "—"}</Link>;
          return text || <span className="muted">—</span>;
        },
      })),
    [columns, resource, zone],
  );
  const data = useMemo<Row[]>(() => rows.map((cells, i) => ({ cells, key: keys[i] ?? "" })), [rows, keys]);
  const table = useReactTable({ data, columns: defs, getCoreRowModel: getCoreRowModel() });
  return (
    <div className="scroll">
      <table className="table">
        <thead>
          {table.getHeaderGroups().map((g) => (
            <tr key={g.id}>
              {g.headers.map((h) => (
                <th key={h.id} scope="col" aria-sort={ariaSort(sort, h.column.id)}>
                  {onSort && sortable.includes(h.column.id) ? (
                    <button type="button" className="sort" onClick={() => onSort(h.column.id)}>
                      {flexRender(h.column.columnDef.header, h.getContext())}
                      <span aria-hidden="true">{ariaSort(sort, h.column.id) === "ascending" ? " ▲" : ariaSort(sort, h.column.id) === "descending" ? " ▼" : ""}</span>
                    </button>
                  ) : (
                    flexRender(h.column.columnDef.header, h.getContext())
                  )}
                </th>
              ))}
            </tr>
          ))}
        </thead>
        <tbody>
          {table.getRowModel().rows.map((r) => (
            <tr key={r.id}>
              {r.getVisibleCells().map((c) => (
                <td key={c.id}>{flexRender(c.column.columnDef.cell, c.getContext())}</td>
              ))}
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
