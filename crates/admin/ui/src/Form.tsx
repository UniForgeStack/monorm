import * as React from "react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "./api";
import { PackageWidget, packageWidget, versionProblem, widgetCall, widgetStream } from "./Packages";
import { cell, fromInput, toInput, widgetFor, type Input, type Zone } from "./format";
import { Link, blockNavigation, detailUrl, editUrl, listBack, navigate, newUrl, useSearch } from "./router";
import { Chips, FileField, Picker } from "./relations";
import { ValueControl } from "./controls";
import { InlineSections, planInlines, startInlines, type InlineState } from "./Inlines";
import { InputFields, inputBody, startInputs, type InputValues } from "./Inputs";
import { ActionDialog, needsDialog, request, toast, useFinish } from "./Actions";
import { Alert, Button, message } from "./ui";
import { Collapsible } from "./components/ui/collapsible";
import { Label } from "./components/ui/label";
import { cn } from "./lib/utils";

type Problems = { fields: Record<string, string[]>; form: string[] };
type Action = "save" | "continue" | "add";
/** A `submit` action runs on the saved record, after the save. */
type Then = api.ActionInfo | undefined;

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
      <FormBody key={`${q.dataUpdatedAt}:${search}`} meta={meta} columns={columns} cells={cells} rows={id === undefined ? undefined : (q.data as api.Detail).inlines} id={id} zone={zone} site={site} reload={() => q.refetch()} />
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

/**
 * The component of a resource that replaces the form.
 * A module that exports `adminMajor` is a component (the contract of Packages.tsx).
 * One that does not is the older `(React) => Component` factory.
 */
function useCustom(meta: api.Resource, site: api.Site | undefined) {
  return useQuery({
    queryKey: ["form-module", meta.name, meta.form_module ?? null],
    enabled: meta.form_component === true,
    retry: false,
    staleTime: Infinity,
    gcTime: Infinity,
    queryFn: async () => {
      const url = meta.form_module ? api.packageUrl(meta.form_module) : api.formModuleUrl(meta.name);
      const module = await import(/* @vite-ignore */ url);
      if (module.adminMajor !== undefined || meta.form_module) {
        const problem = versionProblem(`The form of ${meta.name}`, module, site?.admin_major ?? 0);
        if (problem) throw new Error(problem);
        return { Component: module.default as React.ComponentType<FormProps> };
      }
      return { Component: (module.default as (r: typeof React) => React.ComponentType<FormProps>)(React) };
    },
  });
}

/** The card look of a section: the element differs (a div, a details or a fieldset), so the look sits in one constant. */
const sectionCard = "mb-4 rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs";
const rowClass = "mb-4 grid gap-1.5 last:mb-0";
const KEEP = "You have unsaved changes on this form. Leave without saving?";
const KEEP_NEW = "You have unsaved changes in this new record. Discard them?";

function Block({ section, fields, children }: { section: api.Section; fields: string[]; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  // A field error inside a collapsed section opens it.
  const bad = fields.length > 0;
  useEffect(() => {
    if (bad) setOpen(true);
  }, [bad]);
  if (section.title === null) return <div className={sectionCard}>{children}</div>;
  if (section.collapsed) {
    return (
      <Collapsible className={sectionCard} title={section.title} open={open} onOpenChange={setOpen}>
        {children}
      </Collapsible>
    );
  }
  return (
    <fieldset className={cn(sectionCard, "[&>legend+*]:clear-both")}>
      <legend className="float-left mb-4 w-full p-0 text-base font-semibold">{section.title}</legend>
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
  const typed = useRef(false);
  useEffect(() => {
    ref.current?.showModal();
  }, []);
  // Escape and Cancel discard typed values: they ask first, like the unsaved guard of a form.
  const close = () => {
    if (!typed.current || window.confirm(KEEP_NEW)) done(null);
  };
  return createPortal(
    <dialog
      ref={ref}
      className="dialog m-auto max-h-[90vh] rounded-xl bg-background text-foreground"
      aria-labelledby="dialog-title"
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
    >
      <h2 id="dialog-title" className="mb-4 mt-0 text-lg font-semibold normal-case tracking-normal text-foreground">New {target.title}</h2>
      <FormBody
        meta={target}
        columns={target.fields}
        cells={{}}
        zone={zone}
        site={site}
        reload={() => {}}
        modal={{ done: (key) => void named(target, relation, key).then(done), cancel: close, typed }}
      />
    </dialog>,
    document.body,
  );
}

/** `typed` is set by the form body: true while a field holds a value that the user changed. */
type Modal = { done: (key: string) => void; cancel: () => void; typed: { current: boolean } };

function FormBody({ meta, columns, cells, rows, id, zone, site, reload, modal }: { meta: api.Resource; columns: api.Column[]; cells: Record<string, unknown>; rows?: Record<string, api.InlineRows>; id?: string; zone: Zone; site: api.Site | undefined; reload: () => void; modal?: Modal }) {
  const client = useQueryClient();
  const creating = id === undefined;
  const fields = (meta.form ?? []).flatMap((f) => {
    const column = columns.find((c) => c.label === f.name);
    return column ? [{ f, column, w: widgetFor(f.widget ?? column.widget, column) }] : [];
  });
  const search = useSearch();
  // The modal is a create form that no URL fills.
  const params = new URLSearchParams(modal ? "" : search);
  // A package widget holds a raw JSON value, not a text input.
  const own = (w: string) => packageWidget(site, w) !== undefined;
  const rawStart = (label: string): unknown => (creating ? null : (cells[label] ?? null));
  const blank = (c: api.Column, w: string): Input => toInput(creating ? null : cells[c.label], w, zone);
  const start = () =>
    Object.fromEntries(fields.map(({ column: c, w }) => [c.label, creating && params.has(c.label) ? (w === "checkbox" ? params.get(c.label) === "true" : (params.get(c.label) as string)) : blank(c, w)]));
  const [vals, setVals] = useState<Record<string, Input>>(start);
  const [raws, setRaws] = useState<Record<string, unknown>>(() => Object.fromEntries(fields.filter(({ w }) => own(w)).map(({ column: c }) => [c.label, rawStart(c.label)])));
  // The message that a widget reports with `onError`. A field with a message does not save.
  const [widgetErrors, setWidgetErrors] = useState<Record<string, string>>({});
  const [problems, setProblems] = useState<Problems>({ fields: {}, form: [] });
  const [conflicts, setConflicts] = useState<NonNullable<api.ErrorBody["conflicts"]>>({});
  const [busy, setBusy] = useState(false);
  const finish = useFinish();
  const [afterSave, setAfterSave] = useState<{ action: api.ActionInfo; key: string } | null>(null);
  const submitActions = (meta.actions ?? []).filter((a) => a.placement === "submit");
  const [ins, setIns] = useState<InputValues>(() => startInputs(meta.inputs));
  const custom = useCustom(meta, site);
  // The inline rows: a modal has none, and a custom form does not show them.
  const inlined = !modal && !meta.form_component;
  const [inl, setInl] = useState<InlineState>(() => (inlined ? startInlines(meta, rows, zone) : {}));
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
  const dirty = (c: api.Column, w: string) => (own(w) ? JSON.stringify(raws[c.label] ?? null) !== JSON.stringify(rawStart(c.label)) : vals[c.label] !== blank(c, w));

  /** The one write of both paths. The guard values go along even for a field that the form cannot change. */
  const send = async (set: api.Values, action: Action, changes?: api.Links, inlines?: api.InlineChanges, then?: Then): Promise<boolean> => {
    const loaded: api.Values = {};
    if (!creating) for (const { column: c } of fields) loaded[c.label] = c.label in uploaded ? uploaded[c.label] : cells[c.label];
    setConflicts({});
    setProblems({ fields: {}, form: [] });
    setBusy(true);
    try {
      const done = creating ? await api.create(meta.name, set, changes, inputBody(meta.inputs, ins), inlines) : await api.update(meta.name, id, set, loaded, changes, inputBody(meta.inputs, ins), inlines);
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      await client.invalidateQueries({ queryKey: ["detail"] });
      await client.invalidateQueries({ queryKey: ["autocomplete"] });
      if (modal) {
        modal.done(done.key);
      } else if (then && needsDialog(then)) {
        // The action asks first: the dialog opens on the saved record and the page goes on from there.
        setAfterSave({ action: then, key: done.key });
      } else if (then) {
        go(detailUrl(meta.name, done.key));
        try {
          await finish((await request(meta.name, then, { keys: [done.key], what: "this record" }, {}, false)).result);
        } catch (e) {
          // The record is saved: only the action failed.
          toast(`The record is saved, but ${then.label} failed: ${message(e)}`, "error");
        }
      } else if (action === "add") {
        // Another record of the same kind: the form is empty again, and the prefill of the URL stays.
        if (creating) {
          setVals(start());
          setRaws(Object.fromEntries(fields.filter(({ w }) => own(w)).map(({ column: c }) => [c.label, null])));
          setWidgetErrors({});
          setSaved(empty());
          setChips(empty());
          setIns(startInputs(meta.inputs));
          setInl(startInlines(meta, undefined, zone));
        } else go(newUrl(meta.name));
      } else if (action === "continue") {
        if (creating) go(editUrl(meta.name, done.key));
        else reload();
      } else go(detailUrl(meta.name, done.key));
      return true;
    } catch (e) {
      if (!(e instanceof api.ApiError)) setProblems({ fields: {}, form: [message(e)] });
      else {
        if (e.status === 409) setConflicts(e.body.conflicts ?? {});
        // A failure with field messages shows under the summary: the generic server message would repeat it.
        const fields = e.body.fields ?? {};
        const named = Object.values(fields).some((m) => m.length > 0);
        setProblems({ fields, form: e.body.form?.length ? e.body.form : e.status === 409 || named ? [] : [e.message] });
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const submit = async (action: Action, then?: Then) => {
    const set: api.Values = {};
    const local: Record<string, string[]> = {};
    for (const { f, column: c, w } of fields) {
      // A read-only field is never sent: the server would refuse it.
      if (f.readonly || (!creating && f.readonly_on_update)) continue;
      // A checkbox that nothing fills and no default covers goes along even when it is off.
      const needed = creating && w === "checkbox" && f.required && !f.default;
      if (!dirty(c, w) && !needed) continue;
      if (own(w)) {
        if (widgetErrors[c.label]) local[c.label] = [widgetErrors[c.label] as string];
        else set[c.label] = raws[c.label] ?? null;
        continue;
      }
      const out = fromInput(vals[c.label] ?? "", w, c.nullable, zone);
      if ("error" in out) local[c.label] = [out.error];
      else set[c.label] = out.value;
    }
    const plan = inlined ? planInlines(meta, inl, zone) : { body: undefined, local: {} };
    Object.assign(local, plan.local);
    setConflicts({});
    if (Object.keys(local).length > 0) return setProblems({ fields: local, form: [] });
    await send(set, action, links(), plan.body, then);
  };
  const on = (action: Action) => (e: FormEvent) => {
    e.preventDefault();
    // The modal sits in the React tree of the open form: its submit must not reach that form.
    e.stopPropagation();
    void submit(action);
  };
  const set = (name: string, v: Input) => setVals((s) => ({ ...s, [name]: v }));

  // Unsaved work: a page change asks first, and so does a reload or a close of the tab. The modal is inside a form that asks already.
  const first = useRef<string | undefined>(undefined);
  const snapshot = JSON.stringify([vals, raws, ins]);
  first.current ??= snapshot;
  const changed = !modal && (first.current !== snapshot || links() !== undefined || (inlined && planInlines(meta, inl, zone).body !== undefined));
  if (modal) modal.typed.current = first.current !== snapshot;
  const leaving = useRef(false);
  const askRef = useRef<() => boolean>(() => true);
  askRef.current = () => leaving.current || !changed || window.confirm(KEEP);
  useEffect(() => {
    if (!changed) return;
    const warn = (e: BeforeUnloadEvent) => {
      if (!leaving.current) e.preventDefault();
    };
    window.addEventListener("beforeunload", warn);
    const unblock = blockNavigation(() => askRef.current());
    return () => {
      window.removeEventListener("beforeunload", warn);
      unblock();
    };
  }, [changed]);
  // The save already wrote the work: the guard would ask about values that are no longer unsaved.
  const go = (to: string) => {
    leaving.current = true;
    navigate(to);
    leaving.current = false;
  };
  const summary = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (Object.keys(problems.fields).length > 0) summary.current?.focus();
  }, [problems]);

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
  const inlineNames = (meta.inlines ?? []).map((i) => `${i.name}.`);
  const unplaced = Object.keys(problems.fields).filter((n) => !inlineNames.some((p) => n.startsWith(p)) && !fields.some(({ column }) => column.label === n) && !(meta.inputs ?? []).some((d) => d.name === n));
  // One line for each field that has a message: a link to the field, or only its name for an inline row or an input.
  const sumItems = [...new Set([...Object.keys(problems.fields).filter((n) => (problems.fields[n]?.length ?? 0) > 0), ...Object.keys(conflicts)])].flatMap((n) => {
    const own_ = fields.find(({ column }) => column.label === n);
    const label = own_ ? own_.column.title : (inlineNames.find((p) => n.startsWith(p))?.slice(0, -1) ?? n);
    return [{ label, target: own_ ? `f-${modal ? "m-" : ""}${n}` : undefined }];
  }).filter((it, i, all) => all.findIndex((x) => x.label === it.label) === i);
  const configured = creating ? meta.sections?.create : meta.sections?.edit;
  // The fields that no section names come last without a title: beside titled cards they get "Other", as on the detail page.
  const layout = configured?.map((s) => (s.title === null && configured.length > 1 ? { ...s, title: "Other" } : s)) ?? [{ title: null, fields: fields.map(({ column }) => column.label), collapsed: false }];

  const input = ({ f, column: c, w }: (typeof fields)[number]) => {
    const name = c.label;
    const locked = f.readonly || (!creating && f.readonly_on_update);
    const errors = problems.fields[name] ?? [];
    const conflict = conflicts[name];
    const id_ = `f-${modal ? "m-" : ""}${name}`;
    const common = { id: id_, name, disabled: locked, "aria-invalid": errors.length > 0 || undefined, "aria-describedby": `${id_}-msg` };
    const value = vals[name] ?? "";
    return (
      <div key={name} className={cn(rowClass, (errors.length > 0 || conflict) && "field-bad")}>
        <Label id={`${id_}-label`} htmlFor={id_}>
          {c.title}
          {f.required && creating && !f.readonly && <span className="text-destructive" title="Required"> *</span>}
          {f.default && creating && <span className="font-normal text-muted-foreground"> (default)</span>}
          {f.readonly && <span className="font-normal text-muted-foreground"> (read-only)</span>}
        </Label>
        {own(w) && site ? (
          <PackageWidget
            site={site}
            w={w}
            id={id_}
            name={name}
            value={raws[name] ?? null}
            onChange={(v) => setRaws((r) => ({ ...r, [name]: v }))}
            onError={(m) => setWidgetErrors((e) => (m ? { ...e, [name]: m } : Object.fromEntries(Object.entries(e).filter(([k]) => k !== name))))}
            field={{ name, title: c.title, type: c.type, array: c.array ?? false, required: f.required ?? false, nullable: c.nullable, choices: c.choices ?? [], max_length: f.max_length ?? null }}
            record={Object.fromEntries(fields.map(({ column }) => [column.label, rawStart(column.label)]))}
            readOnly={locked ?? false}
            disabled={busy}
            errors={errors}
            call={widgetCall}
            stream={widgetStream}
          />
        ) : f.relation ? (
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
        ) : w === "radio" ? (
          <div id={id_} role="radiogroup" tabIndex={-1} aria-labelledby={`${id_}-label`} aria-invalid={common["aria-invalid"]} aria-describedby={common["aria-describedby"]} className="flex flex-wrap gap-x-4 gap-y-1 rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring">
            {(c.nullable || creating ? ["", ...(c.choices ?? [])] : (c.choices ?? [])).map((v) => (
              <label key={v} className="inline-flex items-center gap-1.5 text-sm">
                <input type="radio" name={name} disabled={locked} value={v} checked={value === v} onChange={() => set(name, v)} /> {v === "" ? "—" : v}
              </label>
            ))}
          </div>
        ) : (
          <ValueControl w={w} column={c} value={value} set={(v) => set(name, v)} blank={c.nullable || creating ? "—" : "Choose…"} id={id_} name={name} disabled={locked} invalid={errors.length > 0} described={common["aria-describedby"]} maxLength={f.max_length} rows={w === "textarea" ? 4 : w === "json" ? 10 : 6} />
        )}
        <div id={`${id_}-msg`}>
          {errors.map((m) => <p key={m} className="msg msg-error m-0 text-sm text-destructive">{m}</p>)}
          {conflict && (
            <p className="msg msg-error m-0 text-sm text-destructive">
              Changed to <code>{cell(conflict.current, c, zone) || "(empty)"}</code> since you opened this record.
            </p>
          )}
        </div>
      </div>
    );
  };

  return (
    <form className={modal ? "grid" : "grid max-w-2xl"} onSubmit={on("save")} noValidate>
      {stale && (
        <Alert>
          This record changed after you opened it. The current value shows under each field that changed.{" "}
          <button type="button" className="cursor-pointer text-link underline" onClick={reload}>Reload the record</button> (your edits are lost).
        </Alert>
      )}
      {sumItems.length > 0 && (
        <div ref={summary} role="group" tabIndex={-1} aria-labelledby={`${modal ? "m" : "f"}-problems`} className="mb-4 rounded-xl border border-destructive bg-card p-4 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring">
          <h2 id={`${modal ? "m" : "f"}-problems`} className="m-0 text-sm font-semibold tracking-normal text-destructive normal-case">
            {sumItems.length === 1 ? "1 field needs attention" : `${sumItems.length} fields need attention`}
          </h2>
          <ul className="m-0 mt-1 list-disc pl-5">
            {sumItems.map((it) => (
              <li key={it.label}>
                {it.target ? (
                  <a href={`#${it.target}`} onClick={(e) => (e.preventDefault(), document.getElementById(it.target as string)?.focus())}>{it.label}</a>
                ) : (
                  it.label
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
      {problems.form.map((m) => <Alert key={m}>{m}</Alert>)}
      {unplaced.map((n) => <Alert key={n}>{n}: {(problems.fields[n] ?? []).join(" ")}</Alert>)}
      {layout.map((section, i) => {
        const inside = section.fields.flatMap((n) => fields.filter(({ column }) => column.label === n));
        if (inside.length === 0) return null;
        const bad = inside.map(({ column }) => column.label).filter((n) => (problems.fields[n]?.length ?? 0) > 0 || conflicts[n]);
        return <Block key={`${section.title ?? ""}:${i}`} section={section} fields={bad}>{inside.map(input)}</Block>;
      })}
      {(meta.inputs?.length ?? 0) > 0 && <InputFields defs={meta.inputs ?? []} vals={ins} errors={problems.fields} onChange={(n, v) => setIns((s) => ({ ...s, [n]: v }))} scope={modal ? "m" : "f"} />}
      {chipRelations.map((r) => (
        <div key={r.name} className={rowClass}>
          <Label htmlFor={`f-chips-${r.name}`}>{r.name}</Label>
          <Chips
            id={`f-chips-${r.name}`}
            resource={meta.name}
            relation={r}
            items={chips[r.name]}
            onChange={(items) => setChips((s) => ({ ...s, [r.name]: items }))}
            onCreate={mayCreate(r)}
          />
          {capped.includes(r.name) && <p className="m-0 text-sm text-muted-foreground">Only the first 200 links show. The others stay as they are.</p>}
        </div>
      ))}
      {inlined && <InlineSections meta={meta} state={inl} setState={setInl} errors={problems.fields} conflicts={conflicts} zone={zone} site={site} parentKey={id} />}
      <div className="savebar sticky bottom-0 z-10 mt-2 flex flex-wrap items-center gap-2 rounded-xl border border-border bg-card/95 p-3 shadow-md backdrop-blur">
        <Button type="submit" disabled={busy}>Save</Button>
        {modal ? (
          <Button type="button" variant="plain" disabled={busy} onClick={modal.cancel}>Cancel</Button>
        ) : (
          <>
            <Button type="button" variant="plain" disabled={busy} onClick={on("continue")}>Save and continue</Button>
            <Button type="button" variant="plain" disabled={busy} onClick={on("add")}>Save and add another</Button>
            {submitActions.map((a) => (
              <Button key={a.name} type="button" variant="plain" disabled={busy} onClick={(e) => { e.preventDefault(); void submit("save", a); }}>Save and {a.label.toLowerCase()}</Button>
            ))}
            <Link href={creating ? listBack(meta.name) : detailUrl(meta.name, id)} className="ml-auto text-sm">Cancel</Link>
          </>
        )}
      </div>
      {afterSave && <ActionDialog key={afterSave.action.name} resource={meta.name} action={afterSave.action} target={{ keys: [afterSave.key], what: "this record" }} onClose={() => { const key = afterSave.key; setAfterSave(null); go(detailUrl(meta.name, key)); }} />}
      {creator && (() => {
        const target = site?.resources.find((r) => r.name === creator.relation.target);
        return target ? <CreateDialog target={target} relation={creator.relation} zone={zone} site={site} done={creator.done} /> : null;
      })()}
    </form>
  );
}
