import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type ReactNode } from "react";
import * as api from "./api";
import { Input as TextInput, Select, Textarea } from "./components/ui/input";
import { cn } from "./lib/utils";
import { cell, fromInput, inputType, toInput, type Input, type Zone } from "./format";
import { blockNavigation } from "./router";
import { Alert, Button, message } from "./ui";

/** The text of each edited cell, by the encoded key of its row and the label of its column. */
type Changes = Record<string, Record<string, Input>>;

/** What the list shows for one cell that the user may edit. */
export type EditFn = (column: api.Column, row: number, key: string, value: unknown) => ReactNode;

const KEEP = "You have unsaved changes in this list. Discard them?";

/** The control of one cell. `data-edit` lets the table move the focus from cell to cell with Tab. */
function EditCell({ id, column, w, value, label, changed, errors, rowErrors, stored, onChange, onUseStored }: {
  id: string;
  column: api.Column;
  w: string;
  value: Input;
  label: string;
  changed: boolean;
  errors: string[];
  /** The problems of the whole row, shown under its first editable cell. */
  rowErrors: string[];
  stored: { text: string } | undefined;
  onChange: (v: Input) => void;
  onUseStored: () => void;
}) {
  const common = {
    id,
    name: column.label,
    "data-edit": "",
    "data-changed": changed || undefined,
    "aria-label": `${column.title} of ${label}`,
    "aria-invalid": errors.length > 0 || stored !== undefined || undefined,
    "aria-describedby": errors.length + rowErrors.length > 0 || stored ? `${id}-msg` : undefined,
  };
  const mark = cn("h-8 min-w-24 py-1", changed && "border-primary bg-accent");
  let control: ReactNode;
  if (w === "checkbox") control = <input {...common} type="checkbox" checked={value === true} onChange={(e) => onChange(e.target.checked)} />;
  else if (w === "select" || w === "radio") {
    control = (
      <Select {...common} className={mark} value={String(value)} onChange={(e) => onChange(e.target.value)}>
        <option value="">{column.nullable ? "—" : "Choose…"}</option>
        {column.choices?.map((v) => <option key={v} value={v}>{v}</option>)}
      </Select>
    );
  } else if (w === "textarea" || w === "json" || w === "list") control = <Textarea {...common} className={cn(mark, "min-h-8")} rows={1} value={String(value)} onChange={(e) => onChange(e.target.value)} />;
  else control = <TextInput {...common} className={mark} type={inputType(w)} step={w === "datetime" ? 1 : undefined} inputMode={w === "number" ? "decimal" : undefined} value={String(value)} onChange={(e) => onChange(e.target.value)} />;
  return (
    <div className="grid gap-1">
      {control}
      <div id={`${id}-msg`}>
        {errors.map((m) => <p key={m} className="msg msg-error m-0 text-xs">{m}</p>)}
        {rowErrors.map((m) => <p key={m} className="msg msg-error m-0 text-xs" data-row-error="">{m}</p>)}
        {stored && (
          <p className="msg msg-error m-0 text-xs">
            Changed to <strong>{stored.text || "—"}</strong> by someone else.{" "}
            <button type="button" className="underline" onClick={onUseStored}>Use that value</button>
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * The edit of a list in its cells: the changes by row, the save of all of them in one request, and the bar that shows them.
 * `edit` is null while the list is read-only (a tab, or a user with nothing to edit).
 */
export function useListEdit({ resource, meta, page, zone, enabled }: { resource: string; meta: api.Resource | undefined; page: api.Page | undefined; zone: Zone; enabled: boolean }) {
  const client = useQueryClient();
  const [changes, setChanges] = useState<Changes>({});
  const [busy, setBusy] = useState(false);
  // The answer of the last save: its problems name the rows by the position in the request, so the request order stays with them.
  const [sent, setSent] = useState<string[]>([]);
  const [problems, setProblems] = useState<api.ListProblems | null>(null);
  const [local, setLocal] = useState<Record<string, Record<string, string[]>>>({});
  const [formError, setFormError] = useState<string | null>(null);
  // After a conflict, the next save compares against the stored value that the answer named.
  const [stale, setStale] = useState<Record<string, Record<string, unknown>>>({});
  const editable = new Set(enabled ? (meta?.editable ?? []) : []);
  const count = Object.keys(changes).length;

  const reset = () => {
    setChanges({});
    setProblems(null);
    setLocal({});
    setFormError(null);
    setStale({});
    setSent([]);
  };
  // The confirm that every drop of changes shows: a page change, a sort, a filter, a search, a bulk action.
  const ask = () => {
    if (count === 0) return true;
    if (!window.confirm(KEEP)) return false;
    reset();
    return true;
  };
  const askRef = useRef(ask);
  askRef.current = ask;
  useEffect(() => {
    if (count === 0) return;
    const leave = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", leave);
    const unblock = blockNavigation(() => askRef.current());
    return () => {
      window.removeEventListener("beforeunload", leave);
      unblock();
    };
  }, [count === 0]);

  // A new page ends the notes of the last save, and the stored values that it named.
  useEffect(() => {
    if (count !== 0) return;
    setProblems(null);
    setStale({});
    setSent([]);
  }, [page]);

  const widgetOf = (c: api.Column) => meta?.form?.find((f) => f.name === c.label)?.widget ?? c.widget;
  const columnOf = (label: string) => page?.columns.find((c) => c.label === label);

  const put = (key: string, c: api.Column, start: Input, v: Input) =>
    setChanges((all) => {
      const row = { ...(all[key] ?? {}) };
      if (v === start) delete row[c.label];
      else row[c.label] = v;
      const next = { ...all };
      if (Object.keys(row).length === 0) delete next[key];
      else next[key] = row;
      return next;
    });

  // The first editable cell of a row carries the problems that are about the row: a form message, and a field that the list does not edit.
  const first = page?.columns.find((c) => editable.has(c.label))?.label;
  const rowProblems = (at: number) => {
    const p = problems?.rows[String(at)];
    if (!p) return [];
    return [...(p.form ?? []), ...Object.entries(p.fields ?? {}).filter(([label]) => !editable.has(label)).flatMap(([label, ms]) => ms.map((m) => `${label}: ${m}`))];
  };

  const edit: EditFn | null =
    editable.size === 0
      ? null
      : (c, row, key, value) => {
          if (!editable.has(c.label) || page?.flags?.[row]?.change === false) return null;
          const w = widgetOf(c);
          const start = toInput(c.label in (stale[key] ?? {}) ? stale[key]?.[c.label] : value, w, zone);
          const at = sent.indexOf(key);
          const conflict = at >= 0 ? problems?.conflicts[String(at)]?.[c.label] : undefined;
          const errors = [...(at >= 0 ? (problems?.rows[String(at)]?.fields?.[c.label] ?? []) : []), ...(local[key]?.[c.label] ?? [])];
          const changed = c.label in (changes[key] ?? {});
          const rowErrors = c.label === first && at >= 0 ? rowProblems(at) : [];
          return (
            <EditCell
              id={`e-${key}-${c.label}`}
              column={c}
              w={w}
              label={decodeURIComponent(key)}
              value={changes[key]?.[c.label] ?? start}
              changed={changed}
              errors={errors}
              rowErrors={rowErrors}
              stored={conflict ? { text: cell(conflict.current, c, zone) } : undefined}
              onChange={(v) => put(key, c, start, v)}
              onUseStored={() => {
                setChanges((all) => {
                  const next = { ...all, [key]: { ...(all[key] ?? {}) } };
                  delete next[key]?.[c.label];
                  if (Object.keys(next[key] ?? {}).length === 0) delete next[key];
                  return next;
                });
                setStale((s) => ({ ...s, [key]: { ...s[key], [c.label]: conflict?.current } }));
                // The conflict is settled: the cell shows the stored value and no longer names a conflict.
                setProblems((p) => {
                  if (!p) return p;
                  const { [c.label]: _gone, ...rest } = p.conflicts[String(at)] ?? {};
                  const conflicts = { ...p.conflicts, [String(at)]: rest };
                  if (Object.keys(rest).length === 0) delete conflicts[String(at)];
                  return { ...p, conflicts };
                });
              }}
            />
          );
        };

  const save = async () => {
    if (!page) return;
    const rows: { key: string; set: api.Values; loaded: api.Values }[] = [];
    const bad: Record<string, Record<string, string[]>> = {};
    for (const [key, cells] of Object.entries(changes)) {
      const i = page.keys.indexOf(key);
      if (i < 0) continue;
      const set: api.Values = {};
      for (const [label, text] of Object.entries(cells)) {
        const c = columnOf(label);
        if (!c) continue;
        const out = fromInput(text, widgetOf(c), c.nullable, zone);
        if ("error" in out) bad[key] = { ...bad[key], [label]: [out.error] };
        else set[label] = out.value;
      }
      // Every list column goes along: the server compares the fields of `set` and the guard fields.
      const loaded: api.Values = Object.fromEntries(page.columns.map((c, n) => [c.label, key in stale && c.label in (stale[key] ?? {}) ? stale[key]?.[c.label] : page.rows[i]?.[n]]));
      rows.push({ key, set, loaded });
    }
    setLocal(bad);
    if (Object.keys(bad).length > 0 || rows.length === 0) return;
    setBusy(true);
    setFormError(null);
    setProblems(null);
    const order = rows.map((r) => r.key);
    try {
      await api.saveRows(resource, rows);
      await client.invalidateQueries({ queryKey: ["list", resource] });
      await client.invalidateQueries({ queryKey: ["count"] });
      await client.invalidateQueries({ queryKey: ["detail"] });
      reset();
    } catch (e) {
      const p = api.listProblems(e);
      setSent(order);
      setProblems(p);
      setStale((s) => {
        const next = { ...s };
        for (const [at, fields] of Object.entries(p.conflicts)) {
          const key = order[Number(at)];
          if (key !== undefined) next[key] = { ...next[key], ...Object.fromEntries(Object.entries(fields).map(([f, v]) => [f, v.current])) };
        }
        return next;
      });
      setFormError(Object.keys(p.rows).length + Object.keys(p.conflicts).length === 0 ? message(e) : null);
    } finally {
      setBusy(false);
    }
  };

  const conflicted = Object.keys(problems?.conflicts ?? {}).length > 0;

  const bar =
    count === 0 ? null : (
      <div className="sticky bottom-3 z-10 mt-3 grid gap-2 rounded-lg border border-border bg-accent px-3 py-2 text-sm shadow-xs" role="group" aria-label="Unsaved changes">
        <div className="flex flex-wrap items-center gap-2">
          <span role="status" className="mr-2 font-medium">{count} {count === 1 ? "row" : "rows"} changed</span>
          <Button size="sm" disabled={busy} onClick={() => void save()}>Save</Button>
          <Button variant="plain" size="sm" disabled={busy} onClick={reset}>Discard</Button>
        </div>
        {formError && <Alert>{formError}</Alert>}
        {conflicted && <p className="m-0 text-destructive">Another user changed some rows after this list loaded. The cells show the stored value. Save again to keep yours.</p>}
        {Object.keys(problems?.rows ?? {}).length > 0 && <p className="m-0 text-destructive">Nothing was saved. Fix the marked cells and save again.</p>}
      </div>
    );

  return { edit, bar, ask };
}
