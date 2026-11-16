import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, useSyncExternalStore } from "react";
import * as api from "./api";
import { InputFields, type InputValues, startInputs } from "./Inputs";
import { BASE, navigate } from "./router";
import { Alert, Button, message } from "./ui";

type Level = "info" | "success" | "warning" | "error";
interface Toast {
  id: number;
  text: string;
  level: Level;
}

let toasts: Toast[] = [];
let nextToast = 1;
const watchers = new Set<() => void>();
const publish = (next: Toast[]) => {
  toasts = next;
  watchers.forEach((fn) => fn());
};

/** Show a message from an action. It leaves by itself after eight seconds. */
export function toast(text: string, level: Level = "info") {
  const id = nextToast++;
  publish([...toasts, { id, text, level }]);
  setTimeout(() => publish(toasts.filter((t) => t.id !== id)), 8000);
}

/** The messages of the actions: one place for the whole app. */
export function Toasts() {
  const list = useSyncExternalStore(
    (fn) => (watchers.add(fn), () => void watchers.delete(fn)),
    () => toasts,
  );
  if (list.length === 0) return null;
  return (
    <div className="toasts" aria-live="polite">
      {list.map((t) => (
        <div key={t.id} className={`toast toast-${t.level}`} role={t.level === "error" ? "alert" : "status"}>
          <span>{t.text}</span>
          <button type="button" className="toast-close" aria-label="Dismiss" onClick={() => publish(toasts.filter((x) => x.id !== t.id))}>×</button>
        </div>
      ))}
    </div>
  );
}

/** A path under the admin stays in the app: any other path of the site is a page load. */
const inApp = (to: string) => to === BASE || to.startsWith(`${BASE}/`);

function save(filename: string, type: string, base64: string) {
  const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** What the SPA does with the result of an action. Every kind reloads the data: an action can change any row. */
export function useFinish() {
  const client = useQueryClient();
  return async (result: api.ActionResult) => {
    await client.invalidateQueries({ queryKey: ["list"] });
    await client.invalidateQueries({ queryKey: ["count"] });
    await client.invalidateQueries({ queryKey: ["detail"] });
    if (result.kind === "message") toast(result.message, result.level);
    else if (result.kind === "download") save(result.filename, result.content_type, result.base64);
    else if (result.kind === "redirect") {
      if (inApp(result.to)) navigate(result.to);
      else window.location.assign(result.to);
    }
  };
}

/** The rows of one run: the keys of a selection, all the rows that the URL filters match, or the one row of a page. */
export interface Target {
  keys?: string[];
  all?: boolean;
  /** The filters of the list as the URL holds them: an action on "all rows" reads them there. */
  search?: string;
  scope?: api.Scope;
  /** The run is on the selection of the list: it ends the selection. */
  ofSelection?: boolean;
  /** The words for the dialog: "3 records", "this record". */
  what: string;
}

/** An action as the dialog shows it. The built-in bulk delete has the same shape. */
export function deleteAction(meta: api.Resource): api.ActionInfo {
  return { name: "bulk_delete", label: "Delete", placement: "bulk", confirm: "This cannot be undone.", inputs: meta.inputs ?? [], applies: false };
}

/** An action that needs no question runs at once. A delete always asks. */
export const needsDialog = (a: api.ActionInfo) => a.name === "bulk_delete" || !!a.confirm || a.inputs.length > 0;

const toBase64 = async (file: File) => {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let text = "";
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(text);
};

/** The request of an action on its target. A file input goes as base64. */
export async function request(resource: string, action: api.ActionInfo, target: Target, inputs: api.Values, confirmLarge: boolean) {
  const body: api.ActionBody = { inputs, ...(confirmLarge ? { confirm_large: true } : {}) };
  if (target.keys) body.keys = target.keys;
  if (target.all) body.all = true;
  return api.runAction(resource, action.name, body, target.search ?? "", target.scope);
}

/** The question before an action: its confirmation text, its inputs and, for a selection over 10,000 rows, a second confirmation. */
export function ActionDialog({ resource, action, target, onClose, onDone }: { resource: string; action: api.ActionInfo; target: Target; onClose: () => void; onDone?: (target: Target) => void }) {
  const finish = useFinish();
  const [ins, setIns] = useState<InputValues>(() => startInputs(action.inputs.filter((d) => d.type !== "file")));
  const [files, setFiles] = useState<Record<string, File | null>>({});
  const [bad, setBad] = useState<Record<string, string[]>>({});
  const [problem, setProblem] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [large, setLarge] = useState(false);
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    window.addEventListener("keydown", esc);
    return () => window.removeEventListener("keydown", esc);
  }, [busy, onClose]);
  const plain = action.inputs.filter((d) => d.type !== "file");
  const run = async () => {
    setBusy(true);
    setProblem([]);
    setBad({});
    try {
      const inputs: api.Values = { ...ins };
      for (const d of action.inputs.filter((x) => x.type === "file")) {
        const f = files[d.name];
        inputs[d.name] = f ? { name: f.name, content_type: f.type || "application/octet-stream", base64: await toBase64(f) } : null;
      }
      const done = await request(resource, action, target, inputs, large);
      await finish(done.result);
      onDone?.(target);
      onClose();
    } catch (e) {
      if (e instanceof api.ApiError && e.kind === "ConfirmLarge") setLarge(true);
      else {
        const body = e instanceof api.ApiError ? e.body : {};
        setBad(body.fields ?? {});
        setProblem(body.form?.length ? body.form : Object.keys(body.fields ?? {}).length > 0 ? [] : [message(e)]);
      }
      setBusy(false);
    }
  };
  const label = action.name === "bulk_delete" ? "Delete" : action.label;
  return (
    <div className="confirm" role="alertdialog" aria-label={`Confirm ${label}`}>
      <p>
        <strong>{label}</strong> on {target.what}.{" "}
        {action.confirm}
      </p>
      {plain.length > 0 && <InputFields defs={plain} vals={ins} errors={bad} onChange={(n, v) => setIns((s) => ({ ...s, [n]: v }))} scope={`action-${action.name}`} />}
      {action.inputs.filter((d) => d.type === "file").map((d) => (
        <div key={d.name} className={(bad[d.name] ?? []).length > 0 ? "field field-bad" : "field"}>
          <label htmlFor={`in-action-${action.name}-${d.name}`}>
            {d.label}
            {d.required && <span className="req" title="Required"> *</span>}
          </label>
          <input id={`in-action-${action.name}-${d.name}`} type="file" onChange={(e) => setFiles((s) => ({ ...s, [d.name]: e.target.files?.[0] ?? null }))} />
          {d.help && <p className="muted">{d.help}</p>}
          {(bad[d.name] ?? []).map((m) => <p key={m} className="msg msg-error">{m}</p>)}
        </div>
      ))}
      {large && <Alert>This selection holds more than 10,000 records. Confirm once more to run the action on all of them.</Alert>}
      {problem.map((m) => <Alert key={m}>{m}</Alert>)}
      <Button disabled={busy} onClick={run}>{large ? `Yes, ${label.toLowerCase()} them all` : label}</Button>{" "}
      <Button variant="plain" disabled={busy} onClick={onClose}>Cancel</Button>
    </div>
  );
}

/** Run an action that needs no question, and show a failure as a message. */
export function useQuick(resource: string, onLarge: (action: api.ActionInfo, target: Target) => void, onDone?: (target: Target) => void) {
  const finish = useFinish();
  return async (action: api.ActionInfo, target: Target) => {
    try {
      const done = await request(resource, action, target, {}, false);
      await finish(done.result);
      onDone?.(target);
    } catch (e) {
      // A selection over 10,000 rows needs the second confirmation: the dialog asks for it.
      if (e instanceof api.ApiError && e.kind === "ConfirmLarge") onLarge(action, target);
      else toast(message(e), "error");
    }
  };
}
