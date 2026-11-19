import { flexRender, getCoreRowModel, useReactTable, type ColumnDef } from "@tanstack/react-table";
import { ArrowDown, ArrowUp, ArrowUpRight } from "lucide-react";
import { useMemo, useRef, type KeyboardEvent, type ReactNode } from "react";
import type { Column } from "./api";
import { Skeleton } from "./components/ui/skeleton";
import { Table as KitTable, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./components/ui/table";
import { cell, type Zone } from "./format";
import { Link, detailUrl } from "./router";
import { cn } from "./lib/utils";

interface Row {
  cells: unknown[];
  key: string;
}

export interface TableProps {
  resource: string;
  columns: Column[];
  rows: unknown[][];
  keys: string[];
  zone: Zone;
  /** For each row, the label of the related row of each foreign-key column. */
  labels?: Record<string, unknown>[];
  /** The resources that this user may open: a foreign key links only to one of them. */
  targets?: Set<string>;
  /** Labels of the columns that the user hid. */
  hidden?: Set<string>;
  compact?: boolean;
  busy?: boolean;
  sortable?: string[];
  unindexed?: string[];
  sort?: string;
  onSort?: (label: string) => void;
  select?: { chosen: Set<string>; toggle: (key: string) => void; toggleAll: () => void };
  rowActions?: (index: number, key: string) => ReactNode;
  /** The control of a cell that the user edits, or null for a plain cell. */
  edit?: ((column: Column, row: number, key: string, value: unknown) => ReactNode) | null;
}

/** The text of a related row (its label), or the raw key when the page carries no label. */
function related(value: unknown, label: unknown, column: Column, zone: Zone): string {
  return label === undefined || label === null ? String(value) : cell(label, { ...column, widget: "text", array: false }, zone);
}

const NO_LABELS: Record<string, unknown>[] = [];

/** One column per meta column. The first one links to the detail of its row; a foreign key shows the label of the related row and links to it. */
export function Table({ resource, columns, rows, keys, zone, labels = NO_LABELS, targets, hidden, compact = false, busy = false, sortable = [], unindexed = [], sort = "", onSort, select, rowActions, edit }: TableProps) {
  // The cell function must stay the same function: a new one remounts the cells and the focus leaves the input.
  const editRef = useRef(edit);
  editRef.current = edit;
  const defs = useMemo<ColumnDef<Row>[]>(
    () =>
      columns.map((c, i) => ({
        id: c.label,
        header: c.title,
        cell: ({ row }) => {
          const value = row.original.cells[i];
          const none = c.empty ?? "—";
          const control = editRef.current?.(c, row.index, row.original.key, value);
          if (control) {
            if (i !== 0) return control;
            // The first column links to the record: the link stays next to the control.
            return (
              <div className="flex items-start gap-2">
                <div className="grow">{control}</div>
                <Link href={detailUrl(resource, row.original.key)} aria-label={`Open ${decodeURIComponent(row.original.key)}`} className="mt-1.5"><ArrowUpRight className="size-4" aria-hidden="true" /></Link>
              </div>
            );
          }
          if (c.ref && value !== null && value !== undefined && !Array.isArray(value) && typeof value !== "object") {
            const text = related(value, labels[row.index]?.[c.label], c, zone);
            if (i === 0) return <Link href={detailUrl(resource, row.original.key)}>{text}</Link>;
            return targets?.has(c.ref.resource) ? <Link href={detailUrl(c.ref.resource, encodeURIComponent(String(value)))}>{text}</Link> : text;
          }
          const text = cell(value, c, zone);
          if (i === 0) return <Link href={detailUrl(resource, row.original.key)}>{text || none}</Link>;
          return text || <span className="text-muted-foreground">{none}</span>;
        },
      })),
    [columns, resource, zone, labels, targets],
  );
  const data = useMemo<Row[]>(() => rows.map((cells, i) => ({ cells, key: keys[i] ?? "" })), [rows, keys]);
  const columnVisibility = useMemo(() => Object.fromEntries([...(hidden ?? [])].map((h) => [h, false])), [hidden]);
  const table = useReactTable({ data, columns: defs, getCoreRowModel: getCoreRowModel(), state: { columnVisibility } });
  const all = keys.length > 0 && !!select && keys.every((k) => select.chosen.has(k));
  const pad = compact ? "py-1" : "py-3";
  // Tab and Shift+Tab move from one edited cell to the next, over the links and checkboxes between them.
  const move = (e: KeyboardEvent<HTMLDivElement>) => {
    const from = e.target as HTMLElement;
    if (e.key !== "Tab" || !from.hasAttribute("data-edit")) return;
    const cells = [...e.currentTarget.querySelectorAll<HTMLElement>("[data-edit]:not(:disabled)")];
    const to = cells[cells.indexOf(from) + (e.shiftKey ? -1 : 1)];
    if (!to) return;
    e.preventDefault();
    to.focus();
  };
  return (
    <div aria-busy={busy || undefined} className={cn("transition-opacity", busy && "opacity-60")} onKeyDown={edit ? move : undefined}>
      <KitTable>
        <TableHeader>
          {table.getHeaderGroups().map((g) => (
            <TableRow key={g.id} className="hover:bg-transparent">
              {select && (
                <TableHead scope="col" className="w-10">
                  <input type="checkbox" aria-label="Select all rows of this page" checked={all} onChange={select.toggleAll} />
                </TableHead>
              )}
              {g.headers.map((h) => (
                <TableHead key={h.id} scope="col" aria-sort={ariaSort(sort, h.column.id)}>
                  {onSort && sortable.includes(h.column.id) ? (
                    <button type="button" className="inline-flex items-center gap-1 rounded-sm font-medium hover:text-foreground" title={unindexed.includes(h.column.id) ? "No index: a large table can refuse this sort" : undefined} onClick={() => onSort(h.column.id)}>
                      {flexRender(h.column.columnDef.header, h.getContext())}
                      {ariaSort(sort, h.column.id) === "ascending" && <ArrowUp className="size-3.5" aria-hidden="true" />}
                      {ariaSort(sort, h.column.id) === "descending" && <ArrowDown className="size-3.5" aria-hidden="true" />}
                    </button>
                  ) : (
                    flexRender(h.column.columnDef.header, h.getContext())
                  )}
                </TableHead>
              ))}
              {rowActions && (
                <TableHead scope="col" className="w-10">
                  <span className="sr-only">Actions</span>
                </TableHead>
              )}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.map((r) => (
            <TableRow key={r.id} aria-selected={select?.chosen.has(r.original.key) || undefined}>
              {select && (
                <TableCell className={cn("w-10", pad)}>
                  <input type="checkbox" aria-label={`Select row ${decodeURIComponent(r.original.key)}`} checked={select.chosen.has(r.original.key)} onChange={() => select.toggle(r.original.key)} />
                </TableCell>
              )}
              {r.getVisibleCells().map((c) => (
                <TableCell key={c.id} className={pad}>{flexRender(c.column.columnDef.cell, c.getContext())}</TableCell>
              ))}
              {rowActions && <TableCell className={cn("w-10 text-right", pad)}>{rowActions(r.index, r.original.key)}</TableCell>}
            </TableRow>
          ))}
        </TableBody>
      </KitTable>
    </div>
  );
}

/** Grey rows while the first page loads. */
export function TableSkeleton({ columns = 4, rows = 6, compact = false }: { columns?: number; rows?: number; compact?: boolean }) {
  return (
    <div role="status" aria-label="Loading the records" className="overflow-hidden rounded-lg border border-border bg-card">
      <div className="flex gap-4 border-b border-border bg-muted/50 px-3 py-3">
        {Array.from({ length: columns }, (_, i) => (
          <Skeleton key={i} className="h-3 flex-1" />
        ))}
      </div>
      {Array.from({ length: rows }, (_, r) => (
        <div key={r} className={cn("flex gap-4 border-b border-border px-3 last:border-b-0", compact ? "py-2" : "py-3.5")}>
          {Array.from({ length: columns }, (_, i) => (
            <Skeleton key={i} className="h-4 flex-1" />
          ))}
        </div>
      ))}
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
