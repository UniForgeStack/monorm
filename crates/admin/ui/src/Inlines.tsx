import { useEffect, useState, type Dispatch, type SetStateAction } from "react";
import * as api from "./api";
import { cell, fromInput, toInput, widgetFor, type Input, type Zone } from "./format";
import { ValueControl } from "./controls";
import { Picker } from "./relations";
import { Link, detailUrl } from "./router";
import { Alert, Button, message } from "./ui";

/** One row of an inline on the form. `key` is null for a row that the server does not have yet. */
export interface Row {
  uid: number;
  key: string | null;
  /** The value of each form field as the server sent it. */
  loaded: api.Values;
  vals: Record<string, Input>;
  removed: boolean;
  flags: api.Flags;
  /** The answer of a row-mode save that failed. */
  problems?: api.ErrorBody;
  busy?: boolean;
}

export type InlineState = Record<string, Row[]>;

let counter = 0;

type Field = { f: api.FormField; column: api.Column; w: string };

/** The fields that a row shows: the form of the inline page without the fields that the parent fixes. */
const fieldsOf = (inline: api.InlineInfo): Field[] =>
  (inline.form ?? []).flatMap((f) => {
    const column = inline.fields.find((c) => c.label === f.name);
    return column ? [{ f, column, w: widgetFor(f.widget ?? column.widget, column) }] : [];
  });

const locked = (f: api.FormField, row: Row) => f.readonly || (row.key !== null && f.readonly_on_update);

function made(inline: api.InlineInfo, key: string | null, values: unknown[] | null, flags: api.Flags | undefined, zone: Zone): Row {
  const loaded: api.Values = {};
  for (const { column } of fieldsOf(inline)) loaded[column.label] = values ? values[inline.fields.findIndex((c) => c.label === column.label)] : null;
  const vals = Object.fromEntries(fieldsOf(inline).map(({ column, w }) => [column.label, toInput(loaded[column.label], w, zone)]));
  return { uid: ++counter, key, loaded, vals, removed: false, flags: flags ?? {} };
}

/** The rows that the record has now, for each inline of the page. */
export function startInlines(meta: api.Resource, rows: Record<string, api.InlineRows> | undefined, zone: Zone): InlineState {
  return Object.fromEntries((meta.inlines ?? []).map((i) => [i.name, (rows?.[i.name]?.rows ?? []).map((r) => made(i, r.key, r.values, r.flags, zone))]));
}

/** The values to send for a row: only the changed fields, or all of them for a new row. */
function collect(inline: api.InlineInfo, row: Row, zone: Zone): { set: api.Values; local: Record<string, string[]> } {
  const set: api.Values = {};
  const local: Record<string, string[]> = {};
  for (const { f, column: c, w } of fieldsOf(inline)) {
    if (locked(f, row)) continue;
    const blank = toInput(row.key === null ? null : row.loaded[c.label], w, zone);
    const needed = row.key === null && w === "checkbox" && f.required && !f.default;
    if (row.vals[c.label] === blank && !needed) continue;
    const out = fromInput(row.vals[c.label] ?? "", w, c.nullable, zone);
    if ("error" in out) local[c.label] = [out.error];
    else set[c.label] = out.value;
  }
  return { set, local };
}

const loadedAll = (inline: api.InlineInfo, row: Row): api.Values => Object.fromEntries(fieldsOf(inline).map(({ column }) => [column.label, row.loaded[column.label]]));

/** The position of each new row among the new rows that the save sends: the server names a problem by it. A blank new row is not sent. */
function createdAt(inline: api.InlineInfo, rows: Row[], zone: Zone): Map<number, number> {
  const at = new Map<number, number>();
  for (const r of rows) if (r.key === null && Object.keys(collect(inline, r, zone).set).length > 0) at.set(r.uid, at.size);
  return at;
}

/** The change set of the inlines that save with the parent. `local` holds the fields that no value can fill, by problem key. */
export function planInlines(meta: api.Resource, state: InlineState, zone: Zone): { body: api.InlineChanges | undefined; local: Record<string, string[]> } {
  const body: api.InlineChanges = {};
  const local: Record<string, string[]> = {};
  for (const inline of meta.inlines ?? []) {
    if (inline.save !== "with_parent" || inline.writable === false) continue;
    const part: api.InlineChanges[string] = { create: [], update: [], delete: [] };
    for (const r of state[inline.name] ?? []) {
      if (r.key !== null && r.removed) {
        part.delete.push({ key: r.key, loaded: loadedAll(inline, r) });
        continue;
      }
      const { set, local: bad } = collect(inline, r, zone);
      if (r.key === null) {
        const at = part.create.length;
        for (const [label, m] of Object.entries(bad)) local[`${inline.name}.new.${at}.${label}`] = m;
        if (Object.keys(set).length > 0) part.create.push({ set });
      } else {
        for (const [label, m] of Object.entries(bad)) local[`${inline.name}.${r.key}.${label}`] = m;
        if (Object.keys(set).length > 0) part.update.push({ key: r.key, set, loaded: loadedAll(inline, r) });
      }
    }
    if (part.create.length + part.update.length + part.delete.length > 0) body[inline.name] = part;
  }
  return { body: Object.keys(body).length > 0 ? body : undefined, local };
}

type Cell = { id: string; field: Field; value: Input; disabled: boolean; errors: string[]; inline: api.InlineInfo; site: api.Site | undefined; set: (v: Input) => void };

/** The control of one field of a row. A file field has none here: a change set carries no file. */
function Control({ id, field: { f, column: c, w }, value, disabled, errors, inline, site, set }: Cell) {
  if (f.file) return <span className="muted">Upload it on its own page</span>;
  if (f.relation) {
    return (
      <Picker
        id={id}
        resource={inline.model}
        relation={f.relation}
        target={site?.resources.find((r) => r.name === f.relation?.target)}
        value={String(value)}
        nullable={c.nullable}
        disabled={disabled}
        invalid={errors.length > 0}
        described={`${id}-msg`}
        onChange={set}
      />
    );
  }
  return <ValueControl w={w} column={c} value={value} set={set} blank={c.nullable ? "—" : "Choose…"} id={id} name={c.label} disabled={disabled} invalid={errors.length > 0} described={`${id}-msg`} label={c.title} maxLength={f.max_length} rows={3} />;
}

/** The inlines of a form: for each one, its rows as a table or as stacked blocks, with Add and Remove. */
export function InlineSections({ meta, state, setState, errors, conflicts, zone, site, parentKey }: {
  meta: api.Resource;
  state: InlineState;
  setState: Dispatch<SetStateAction<InlineState>>;
  /** The problems of the parent save, by problem key. */
  errors: Record<string, string[]>;
  conflicts: Record<string, unknown>;
  zone: Zone;
  site: api.Site | undefined;
  /** The encoded key of the open record, or undefined while the record is new. */
  parentKey: string | undefined;
}) {
  const inlines = (meta.inlines ?? []).filter((i) => i.form);
  if (inlines.length === 0) return null;
  return (
    <>
      {inlines.map((i) => (
        <InlineBlock key={i.name} inline={i} meta={meta} rows={state[i.name] ?? []} setRows={(f) => setState((s) => ({ ...s, [i.name]: f(s[i.name] ?? []) }))} errors={errors} conflicts={conflicts} zone={zone} site={site} parentKey={parentKey} />
      ))}
    </>
  );
}

function InlineBlock({ inline, meta, rows, setRows, errors, conflicts, zone, site, parentKey }: {
  inline: api.InlineInfo;
  meta: api.Resource;
  rows: Row[];
  setRows: (f: (rows: Row[]) => Row[]) => void;
  errors: Record<string, string[]>;
  conflicts: Record<string, unknown>;
  zone: Zone;
  site: api.Site | undefined;
  parentKey: string | undefined;
}) {
  const fields = fieldsOf(inline);
  const ops = inline.ops ?? [];
  const writable = inline.writable !== false;
  const row_mode = inline.save === "row";
  const mayAdd = writable && ops.includes("create");
  const live = rows.filter((r) => !r.removed).length;
  const full = live >= inline.max;
  const prefix = `${inline.name}.`;
  const bad = Object.keys(errors).some((k) => k.startsWith(prefix)) || Object.keys(conflicts).some((k) => k.startsWith(prefix));
  const [open, setOpen] = useState(!inline.collapsed);
  // A problem in a collapsed inline opens it.
  useEffect(() => {
    if (bad) setOpen(true);
  }, [bad]);
  const at = createdAt(inline, rows, zone);
  const problemKey = (r: Row, label: string) => (r.key === null ? `${prefix}new.${at.get(r.uid) ?? -1}.${label}` : `${prefix}${r.key}.${label}`);
  const errorsOf = (r: Row, label: string) => [...(r.problems?.fields?.[label] ?? []), ...(errors[problemKey(r, label)] ?? [])];
  const patch = (uid: number, f: (r: Row) => Row) => setRows((all) => all.map((r) => (r.uid === uid ? f(r) : r)));
  const allows = (r: Row, rule: string) => !(inline.can ?? []).includes(rule) || r.flags[rule] === true;
  const editable = (r: Row) => writable && (r.key === null || (ops.includes("update") && allows(r, "change")));
  const removable = (r: Row) => r.key === null || (writable && inline.delete && ops.includes("delete") && allows(r, "delete"));

  const add = () => setRows((all) => [...all, made(inline, null, null, undefined, zone)]);
  const remove = (r: Row) => (r.key === null ? setRows((all) => all.filter((x) => x.uid !== r.uid)) : patch(r.uid, (x) => ({ ...x, removed: !x.removed })));

  /** Row mode: one row goes to the server alone. The row then takes the stored values, so a second save compares against them. */
  const settle = async (r: Row, run: () => Promise<{ key: string } | null>) => {
    if (parentKey === undefined) return;
    patch(r.uid, (x) => ({ ...x, busy: true, problems: undefined }));
    try {
      const done = await run();
      if (done === null) return setRows((all) => all.filter((x) => x.uid !== r.uid));
      const fresh = (await api.detail(meta.name, parentKey)).inlines?.[inline.name]?.rows.find((x) => x.key === done.key);
      patch(r.uid, () => ({ ...(fresh ? made(inline, fresh.key, fresh.values, fresh.flags, zone) : r), uid: r.uid, key: done.key, busy: false }));
    } catch (e) {
      const body = e instanceof api.ApiError ? e.body : { form: [message(e)] };
      const stale = Object.keys(body.conflicts ?? {}).length > 0;
      patch(r.uid, (x) => ({ ...x, busy: false, problems: stale ? { form: ["This row changed after you opened it. Reload the page and check it."] } : body }));
    }
  };
  const saveRow = (r: Row) => {
    const { set, local } = collect(inline, r, zone);
    if (Object.keys(local).length > 0) return patch(r.uid, (x) => ({ ...x, problems: { fields: local } }));
    if (Object.keys(set).length === 0) return;
    return settle(r, () => (r.key === null ? api.rowCreate(meta.name, parentKey as string, inline.name, set) : api.rowUpdate(meta.name, parentKey as string, inline.name, r.key, set, loadedAll(inline, r))));
  };
  const deleteRow = (r: Row) => {
    if (r.key === null) return remove(r);
    const key = r.key;
    return settle(r, async () => (await api.rowDestroy(meta.name, parentKey as string, inline.name, key, loadedAll(inline, r)), null));
  };
  const dirty = (r: Row) => Object.keys(collect(inline, r, zone).set).length > 0;

  const control = (r: Row, x: Field) => (
    <Control
      id={`i-${inline.name}-${r.uid}-${x.column.label}`}
      field={x}
      value={r.vals[x.column.label] ?? ""}
      disabled={!editable(r) || locked(x.f, r) || r.removed}
      errors={errorsOf(r, x.column.label)}
      inline={inline}
      site={site}
      set={(v) => patch(r.uid, (y) => ({ ...y, vals: { ...y.vals, [x.column.label]: v } }))}
    />
  );
  const messages = (r: Row, label: string) => (
    <div id={`i-${inline.name}-${r.uid}-${label}-msg`}>
      {errorsOf(r, label).map((m) => <p key={m} className="msg msg-error">{m}</p>)}
      {(problemKey(r, label) in conflicts) && <p className="msg msg-error">Changed since you opened this record.</p>}
    </div>
  );
  const actions = (r: Row) => (
    <div className="inline-actions">
      {r.key !== null && inline.link && <Link href={detailUrl(inline.link, r.key)}>Open</Link>}
      {row_mode && parentKey !== undefined && editable(r) && <Button type="button" variant="plain" disabled={r.busy || !dirty(r)} onClick={() => void saveRow(r)}>Save row</Button>}
      {removable(r) && (
        <Button type="button" variant="plain" disabled={r.busy} onClick={() => (row_mode && r.key !== null ? void deleteRow(r) : remove(r))}>
          {r.removed ? "Keep" : r.key === null ? "Remove" : "Delete"}
        </Button>
      )}
    </div>
  );
  const rowProblems = (r: Row) => (r.problems?.form ?? []).map((m) => <Alert key={m}>{m}</Alert>);

  const body = (
    <>
      {!writable && inline.read_only_reason && <p className="muted" role="note">Read-only here: {inline.read_only_reason}</p>}
      {row_mode && parentKey === undefined && mayAdd && <p className="muted">Save the record first. Then add {inline.label.toLowerCase()} one row at a time.</p>}
      {inline.style === "table" && rows.length > 0 ? (
        <table className="inline-table">
          <thead>
            <tr>{fields.map(({ column }) => <th key={column.label} scope="col">{column.title}</th>)}<th scope="col"><span className="sr-only">Row actions</span></th></tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.uid} className={r.removed ? "inline-removed" : undefined}>
                {fields.map((x) => <td key={x.column.label}>{control(r, x)}{messages(r, x.column.label)}</td>)}
                <td>{actions(r)}{rowProblems(r)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        rows.map((r) => (
          <fieldset key={r.uid} className={r.removed ? "section inline-removed" : "section"}>
            {fields.map((x) => (
              <div key={x.column.label} className="field">
                <label htmlFor={`i-${inline.name}-${r.uid}-${x.column.label}`}>{x.column.title}</label>
                {control(r, x)}
                {messages(r, x.column.label)}
              </div>
            ))}
            {actions(r)}
            {rowProblems(r)}
          </fieldset>
        ))
      )}
      {mayAdd && (!row_mode || parentKey !== undefined) && (
        <p>
          <Button type="button" variant="plain" disabled={full} onClick={add}>Add row</Button>
          {full && <span className="muted"> At most {inline.max} rows.</span>}
        </p>
      )}
    </>
  );
  return (
    <details className="section inline" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>{inline.label}</summary>
      {body}
    </details>
  );
}

/** The rows of the inlines on the detail page, read-only. */
export function InlineRowsView({ meta, rows, zone }: { meta: api.Resource; rows: Record<string, api.InlineRows> | undefined; zone: Zone }) {
  return (
    <>
      {(meta.inlines ?? []).map((i) => {
        const got = rows?.[i.name];
        if (!got) return null;
        return (
          <section key={i.name} className="inline-view">
            <h2>{i.label}</h2>
            {got.rows.length === 0 ? (
              <p className="muted">None.</p>
            ) : (
              <table className="inline-table">
                <thead><tr>{i.fields.map((c) => <th key={c.label} scope="col">{c.title}</th>)}{got.link && <th scope="col"><span className="sr-only">Open</span></th>}</tr></thead>
                <tbody>
                  {got.rows.map((r) => (
                    <tr key={r.key}>
                      {i.fields.map((c, n) => <td key={c.label}>{cell(r.values[n], c, zone) || <span className="muted">{c.empty ?? "—"}</span>}</td>)}
                      {got.link && <td><Link href={detailUrl(got.link, r.key)}>Open</Link></td>}
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            {got.truncated && <p className="muted">Only the first {i.max} rows show.</p>}
          </section>
        );
      })}
    </>
  );
}
