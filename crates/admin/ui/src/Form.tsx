import * as React from "react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Label from "@radix-ui/react-label";
import * as api from "./api";
import { cell, type Zone } from "./format";
import { Link, detailUrl, editUrl, listBack, navigate, newUrl, useSearch } from "./router";
import { Chips, FileField, Picker } from "./relations";
import { Alert, Button, message } from "./ui";

type Input = string | boolean;
type Problems = { fields: Record<string, string[]>; form: string[] };
type Action = "save" | "continue" | "add";

const pad = (n: number) => String(n).padStart(2, "0");

/** The text that the input of a field starts with, from the value that the server sent. */
function toInput(value: unknown, w: string, zone: Zone): Input {
  if (w === "checkbox") return value === true;
  if (value === null || value === undefined) return "";
  if (w === "json" || w === "list") return JSON.stringify(value);
  if (w === "datetime" && typeof value === "string") {
    const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    if (Number.isNaN(d.getTime())) return value;
    if (zone === "utc") return d.toISOString().slice(0, 19);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
  return String(value);
}

const STRINGS = ["text", "textarea", "select", "radio", "email", "url"];

/** The value to send for the text of an input, or a message when the text cannot be a value of the field. */
function fromInput(input: Input, w: string, nullable: boolean, zone: Zone): { value: unknown } | { error: string } {
  if (typeof input === "boolean") return { value: input };
  const text = input.trim();
  if (STRINGS.includes(w)) {
    if (text === "" && nullable) return { value: null };
    return { value: w === "text" || w === "textarea" ? input : text };
  }
  if (text === "") return { value: nullable ? null : "" };
  if (w === "json" || w === "list") {
    try {
      return { value: JSON.parse(text) };
    } catch {
      return { error: w === "list" ? "Must be a JSON list, like [1, 2]" : "Must be valid JSON" };
    }
  }
  if (w === "datetime") return { value: zone === "local" ? new Date(text).toISOString() : text };
  return { value: text };
}

const inputType = (w: string) => (w === "datetime" ? "datetime-local" : w === "date" || w === "email" || w === "url" ? w : "text");

/** The form of a record: the create form, or the edit form with the values that the server sent. */
export function FormPage({ resource, id, zone, site }: { resource: string; id?: string; zone: Zone; site: api.Site | undefined }) {
  const search = useSearch();
  const meta = site?.resources.find((r) => r.name === resource);
  // Its own key: a focus or a reconnect must not replace the values that the user typed.
  const q = useQuery({ queryKey: ["form", resource, id], queryFn: () => api.detail(resource, id as string), enabled: id !== undefined, retry: false, gcTime: 0, staleTime: Infinity });
  if (!meta) return <p className="muted">Loading…</p>;
  const back = id === undefined ? listBack(resource) : detailUrl(resource, id);
  const head = (
    <>
      <p><Link href={back}>← {id === undefined ? meta.title : `${meta.title} ${decodeURIComponent(id)}`}</Link></p>
      <h1>{id === undefined ? `Add ${meta.title}` : `Change ${meta.title}`} {id !== undefined && <span className="muted">{decodeURIComponent(id)}</span>}</h1>
    </>
  );
  if (meta.writable === false) return <section>{head}<Alert>{meta.read_only_reason ?? "The admin does not write to this model."}</Alert></section>;
  if (q.isError) return <section>{head}<Alert>{message(q.error)}</Alert></section>;
  if (id !== undefined && !q.data) return <section>{head}<p className="muted">Loading…</p></section>;
  const columns = id === undefined ? meta.fields : (q.data as api.Detail).fields;
  const cells = Object.fromEntries(columns.map((c, i) => [c.label, id === undefined ? null : (q.data as api.Detail).values[i]]));
  // A reload after a conflict gives new values: the key makes the form start again with them.
  return (
    <section>
      {head}
      <FormBody key={`${q.dataUpdatedAt}:${search}`} meta={meta} columns={columns} cells={cells} id={id} zone={zone} site={site} reload={() => q.refetch()} />
    </section>
  );
}

/** What a custom form component gets. `save` sends the values that it gives, and resolves to false when the server refuses them. */
export interface FormProps {
  resource: string;
  creating: boolean;
  /** The value of each form field as the server sent it, or `null` for a new record. */
  values: Record<string, unknown>;
  errors: Problems;
  /** The current value of each field that changed since the form loaded (a 409). */
  conflicts: Record<string, unknown>;
  saving: boolean;
  save: (set: api.Values) => Promise<boolean>;
  reload: () => void;
}

/** The component of a resource that replaces the form: the module default-exports `(React) => Component`. */
function useCustom(meta: api.Resource) {
  return useQuery({
    queryKey: ["form-module", meta.name],
    enabled: meta.form_component === true,
    retry: false,
    staleTime: Infinity,
    gcTime: Infinity,
    queryFn: async () => {
      const module = await import(/* @vite-ignore */ api.formModuleUrl(meta.name));
      return { Component: (module.default as (r: typeof React) => React.ComponentType<FormProps>)(React) };
    },
  });
}

function Block({ section, fields, children }: { section: api.Section; fields: string[]; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  // A field error inside a collapsed section opens it.
  const bad = fields.length > 0;
  useEffect(() => {
    if (bad) setOpen(true);
  }, [bad]);
  if (section.title === null) return <div className="section">{children}</div>;
  if (section.collapsed) {
    return (
      <details className="section" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
        <summary>{section.title}</summary>
        {children}
      </details>
    );
  }
  return (
    <fieldset className="section">
      <legend>{section.title}</legend>
      {children}
    </fieldset>
  );
}

/** The picker row for a record that the modal just created: its key as the server types it, and the text that names it. */
async function named(target: api.Resource, relation: api.RelationInfo, key: string): Promise<api.Option> {
  const fallback = decodeURIComponent(key);
  try {
    const d = await api.detail(target.name, key);
    const at = (name: string | undefined) => d.fields.findIndex((c) => c.label === name);
    const value = d.values[at(target.key[0])] ?? fallback;
    return { value, label: String(d.values[at(relation.label)] ?? value) };
  } catch {
    return { value: fallback, label: fallback };
  }
}

/** The create form of a target record, in a modal over the open form. The open form keeps its values: nothing here touches them. */
function CreateDialog({ target, relation, zone, site, done }: { target: api.Resource; relation: api.RelationInfo; zone: Zone; site: api.Site | undefined; done: (o: api.Option | null) => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    ref.current?.showModal();
  }, []);
  return createPortal(
    <dialog
      ref={ref}
      className="dialog"
      aria-labelledby="dialog-title"
      onCancel={(e) => {
        e.preventDefault();
        done(null);
      }}
    >
      <h2 id="dialog-title">New {target.title}</h2>
      <FormBody
        meta={target}
        columns={target.fields}
        cells={{}}
        zone={zone}
        site={site}
        reload={() => {}}
        modal={{ done: (key) => void named(target, relation, key).then(done), cancel: () => done(null) }}
      />
    </dialog>,
    document.body,
  );
}

type Modal = { done: (key: string) => void; cancel: () => void };

function FormBody({ meta, columns, cells, id, zone, site, reload, modal }: { meta: api.Resource; columns: api.Column[]; cells: Record<string, unknown>; id?: string; zone: Zone; site: api.Site | undefined; reload: () => void; modal?: Modal }) {
  const client = useQueryClient();
  const creating = id === undefined;
  const fields = (meta.form ?? []).flatMap((f) => {
    const column = columns.find((c) => c.label === f.name);
    return column ? [{ f, column, w: f.widget ?? column.widget }] : [];
  });
  const search = useSearch();
  // The modal is a create form that no URL fills.
  const params = new URLSearchParams(modal ? "" : search);
  const blank = (c: api.Column, w: string): Input => toInput(creating ? null : cells[c.label], w, zone);
  const start = () =>
    Object.fromEntries(fields.map(({ column: c, w }) => [c.label, creating && params.has(c.label) ? (w === "checkbox" ? params.get(c.label) === "true" : (params.get(c.label) as string)) : blank(c, w)]));
  const [vals, setVals] = useState<Record<string, Input>>(start);
  const [problems, setProblems] = useState<Problems>({ fields: {}, form: [] });
  const [conflicts, setConflicts] = useState<NonNullable<api.ErrorBody["conflicts"]>>({});
  const [busy, setBusy] = useState(false);
  const custom = useCustom(meta);
  // Many-to-many chips: the saved links and the links on the form. The save sends the difference.
  const chipRelations = modal ? [] : (meta.chips ?? []);
  const empty = () => Object.fromEntries(chipRelations.map((r) => [r.name, [] as api.Option[]]));
  const [saved, setSaved] = useState<Record<string, api.Option[]>>(() => (creating ? empty() : {}));
  const [chips, setChips] = useState<Record<string, api.Option[]>>(() => (creating ? empty() : {}));
  const [capped, setCapped] = useState<string[]>([]);
  // The value that a file field holds after an upload: a later save compares against it, not against the loaded value.
  const [uploaded, setUploaded] = useState<Record<string, unknown>>({});
  const [creator, setCreator] = useState<{ relation: api.RelationInfo; done: (o: api.Option | null) => void } | null>(null);
  useEffect(() => {
    if (creating) return;
    let live = true;
    for (const r of chipRelations) {
      api.links(meta.name, id, r.name).then(
        (got) => {
          if (!live) return;
          setSaved((s) => ({ ...s, [r.name]: got.items }));
          setChips((s) => ({ ...s, [r.name]: got.items }));
          if (got.truncated) setCapped((c) => [...c, r.name]);
        },
        (e) => live && setProblems({ fields: {}, form: [message(e)] }),
      );
    }
    return () => {
      live = false;
    };
    // The saved links load once for a record: a reload remounts the form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const openCreate = (relation: api.RelationInfo) => () => new Promise<api.Option | null>((resolve) => setCreator({ relation, done: (o) => (setCreator(null), resolve(o)) }));
  const mayCreate = (r: api.RelationInfo) => (!modal && r.create && api.may(site?.resources.find((x) => x.name === r.target), "create") ? openCreate(r) : undefined);
  const links = (): api.Links | undefined => {
    const out: api.Links = {};
    for (const r of chipRelations) {
      const now = chips[r.name];
      const was = saved[r.name];
      // A relation that has not loaded has no difference to send.
      if (!now || !was) continue;
      const has = (list: api.Option[], o: api.Option) => list.some((x) => JSON.stringify(x.value) === JSON.stringify(o.value));
      const add = now.filter((o) => !has(was, o)).map((o) => o.value);
      const remove = was.filter((o) => !has(now, o)).map((o) => o.value);
      if (add.length > 0 || remove.length > 0) out[r.name] = { add, remove };
    }
    return Object.keys(out).length > 0 ? out : undefined;
  };
  const dirty = (c: api.Column, w: string) => vals[c.label] !== blank(c, w);

  /** The one write of both paths. The guard values go along even for a field that the form cannot change. */
  const send = async (set: api.Values, action: Action, changes?: api.Links): Promise<boolean> => {
    const loaded: api.Values = {};
    if (!creating) for (const { column: c } of fields) loaded[c.label] = c.label in uploaded ? uploaded[c.label] : cells[c.label];
    setConflicts({});
    setProblems({ fields: {}, form: [] });
    setBusy(true);
    try {
      const done = creating ? await api.create(meta.name, set, changes) : await api.update(meta.name, id, set, loaded, changes);
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      await client.invalidateQueries({ queryKey: ["detail"] });
      await client.invalidateQueries({ queryKey: ["autocomplete"] });
      if (modal) {
        modal.done(done.key);
      } else if (action === "add") {
        // Another record of the same kind: the form is empty again, and the prefill of the URL stays.
        if (creating) {
          setVals(start());
          setSaved(empty());
          setChips(empty());
        } else navigate(newUrl(meta.name));
      } else if (action === "continue") {
        if (creating) navigate(editUrl(meta.name, done.key));
        else reload();
      } else navigate(detailUrl(meta.name, done.key));
      return true;
    } catch (e) {
      if (!(e instanceof api.ApiError)) setProblems({ fields: {}, form: [message(e)] });
      else {
        if (e.status === 409) setConflicts(e.body.conflicts ?? {});
        setProblems({ fields: e.body.fields ?? {}, form: e.body.form?.length ? e.body.form : e.status === 409 ? [] : [e.message] });
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const submit = async (action: Action) => {
    const set: api.Values = {};
    const local: Record<string, string[]> = {};
    for (const { f, column: c, w } of fields) {
      // A read-only field is never sent: the server would refuse it.
      if (f.readonly || (!creating && f.readonly_on_update)) continue;
      // A checkbox that nothing fills and no default covers goes along even when it is off.
      const needed = creating && w === "checkbox" && f.required && !f.default;
      if (!dirty(c, w) && !needed) continue;
      const out = fromInput(vals[c.label] ?? "", w, c.nullable, zone);
      if ("error" in out) local[c.label] = [out.error];
      else set[c.label] = out.value;
    }
    setConflicts({});
    if (Object.keys(local).length > 0) return setProblems({ fields: local, form: [] });
    await send(set, action, links());
  };
  const on = (action: Action) => (e: FormEvent) => {
    e.preventDefault();
    // The modal sits in the React tree of the open form: its submit must not reach that form.
    e.stopPropagation();
    void submit(action);
  };
  const set = (name: string, v: Input) => setVals((s) => ({ ...s, [name]: v }));

  if (meta.form_component) {
    if (custom.isError) return <Alert>The form of this resource did not load: {message(custom.error)}</Alert>;
    if (!custom.data) return <p className="muted">Loading…</p>;
    const Custom = custom.data.Component;
    const values = Object.fromEntries(fields.map(({ column: c }) => [c.label, creating && params.has(c.label) ? params.get(c.label) : cells[c.label]]));
    return (
      <Custom
        resource={meta.name}
        creating={creating}
        values={values}
        errors={problems}
        conflicts={Object.fromEntries(Object.entries(conflicts).map(([k, v]) => [k, v.current]))}
        saving={busy}
        save={(changes) => send(changes, "save")}
        reload={reload}
      />
    );
  }

  const stale = Object.keys(conflicts).length > 0;
  const unplaced = Object.keys(problems.fields).filter((n) => !fields.some(({ column }) => column.label === n));
  const layout = (creating ? meta.sections?.create : meta.sections?.edit) ?? [{ title: null, fields: fields.map(({ column }) => column.label), collapsed: false }];

  const input = ({ f, column: c, w }: (typeof fields)[number]) => {
    const name = c.label;
    const locked = f.readonly || (!creating && f.readonly_on_update);
    const errors = problems.fields[name] ?? [];
    const conflict = conflicts[name];
    const id_ = `f-${modal ? "m-" : ""}${name}`;
    const common = { id: id_, name, disabled: locked, "aria-invalid": errors.length > 0 || undefined, "aria-describedby": `${id_}-msg` };
    const value = vals[name];
    return (
      <div key={name} className={errors.length > 0 || conflict ? "field field-bad" : "field"}>
        <Label.Root id={`${id_}-label`} htmlFor={id_}>
          {c.title}
          {f.required && creating && !f.readonly && <span className="req" title="Required"> *</span>}
          {f.default && creating && <span className="muted"> (default)</span>}
          {f.readonly && <span className="muted"> (read-only)</span>}
        </Label.Root>
        {f.relation ? (
          <Picker
            id={id_}
            resource={meta.name}
            relation={f.relation}
            target={site?.resources.find((r) => r.name === f.relation?.target)}
            value={String(value)}
            nullable={c.nullable}
            disabled={locked}
            invalid={errors.length > 0}
            described={`${id_}-msg`}
            onChange={(v) => set(name, v)}
            onCreate={mayCreate(f.relation)}
          />
        ) : f.file ? (
          <FileField
            id={id_}
            resource={meta.name}
            recordKey={id}
            field={name}
            reference={name in uploaded ? uploaded[name] : cells[name]}
            disabled={locked}
            described={`${id_}-msg`}
            onUploaded={(ref) => {
              setUploaded((u) => ({ ...u, [name]: ref }));
              void client.invalidateQueries({ queryKey: ["detail"] });
            }}
          />
        ) : w === "checkbox" ? (
          <input {...common} type="checkbox" checked={value === true} onChange={(e) => set(name, e.target.checked)} />
        ) : w === "radio" ? (
          <div role="radiogroup" aria-labelledby={`${id_}-label`} aria-invalid={common["aria-invalid"]} aria-describedby={common["aria-describedby"]} className="radios">
            {(c.nullable || creating ? ["", ...(c.choices ?? [])] : (c.choices ?? [])).map((v) => (
              <label key={v} className="radio">
                <input type="radio" name={name} disabled={locked} value={v} checked={value === v} onChange={() => set(name, v)} /> {v === "" ? "—" : v}
              </label>
            ))}
          </div>
        ) : w === "select" ? (
          <select {...common} value={String(value)} onChange={(e) => set(name, e.target.value)}>
            <option value="">{c.nullable || creating ? "—" : "Choose…"}</option>
            {c.choices?.map((v) => <option key={v} value={v}>{v}</option>)}
          </select>
        ) : w === "textarea" || w === "json" || w === "list" ? (
          <textarea {...common} rows={w === "textarea" ? 4 : 6} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} spellCheck={w === "textarea"} />
        ) : (
          <input {...common} type={inputType(w)} step={w === "datetime" ? 1 : undefined} inputMode={w === "number" ? "decimal" : undefined} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} />
        )}
        <div id={`${id_}-msg`}>
          {errors.map((m) => <p key={m} className="msg msg-error">{m}</p>)}
          {conflict && (
            <p className="msg msg-error">
              Changed to <code>{cell(conflict.current, c, zone) || "(empty)"}</code> since you opened this record.
            </p>
          )}
        </div>
      </div>
    );
  };

  return (
    <form className="form" onSubmit={on("save")} noValidate>
      {stale && (
        <Alert>
          This record changed after you opened it. The current value shows under each field that changed.{" "}
          <button type="button" className="linkish" onClick={reload}>Reload the record</button> (your edits are lost).
        </Alert>
      )}
      {problems.form.map((m) => <Alert key={m}>{m}</Alert>)}
      {unplaced.map((n) => <Alert key={n}>{n}: {(problems.fields[n] ?? []).join(" ")}</Alert>)}
      {layout.map((section, i) => {
        const inside = section.fields.flatMap((n) => fields.filter(({ column }) => column.label === n));
        if (inside.length === 0) return null;
        const bad = inside.map(({ column }) => column.label).filter((n) => (problems.fields[n]?.length ?? 0) > 0 || conflicts[n]);
        return <Block key={`${section.title ?? ""}:${i}`} section={section} fields={bad}>{inside.map(input)}</Block>;
      })}
      {chipRelations.map((r) => (
        <div key={r.name} className="field">
          <Label.Root htmlFor={`f-chips-${r.name}`}>{r.name}</Label.Root>
          <Chips
            id={`f-chips-${r.name}`}
            resource={meta.name}
            relation={r}
            items={chips[r.name]}
            onChange={(items) => setChips((s) => ({ ...s, [r.name]: items }))}
            onCreate={mayCreate(r)}
          />
          {capped.includes(r.name) && <p className="muted">Only the first 200 links show. The others stay as they are.</p>}
        </div>
      ))}
      <div className="savebar">
        <Button type="submit" disabled={busy}>Save</Button>
        {modal ? (
          <Button type="button" variant="plain" disabled={busy} onClick={modal.cancel}>Cancel</Button>
        ) : (
          <>
            <Button type="button" variant="plain" disabled={busy} onClick={on("continue")}>Save and continue</Button>
            <Button type="button" variant="plain" disabled={busy} onClick={on("add")}>Save and add another</Button>
            <Link href={creating ? listBack(meta.name) : detailUrl(meta.name, id)} className="cancel">Cancel</Link>
          </>
        )}
      </div>
      {creator && (() => {
        const target = site?.resources.find((r) => r.name === creator.relation.target);
        return target ? <CreateDialog target={target} relation={creator.relation} zone={zone} site={site} done={creator.done} /> : null;
      })()}
    </form>
  );
}
