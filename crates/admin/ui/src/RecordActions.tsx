import { useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { Ellipsis, History, Plus } from "lucide-react";
import * as api from "./api";
import { actionDisabled, ActionDialog, needsDialog, useQuick, type Target } from "./Actions";
import { InputFields, inputBody, startInputs, type InputValues } from "./Inputs";
import { NavIcon } from "./icons";
import { groupActions } from "./lib/actions";
import { StickyFooter } from "./Bars";
import { Link, editUrl, listBack, listUrl, navigate, newUrl } from "./router";
import { ConfirmDialog } from "./components/confirm-dialog";
import { Button, buttonVariants } from "./components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./components/ui/dropdown-menu";
import { Alert, message } from "./ui";

const VARIANT_LOOK: Record<api.ActionVariant, "outline" | "default" | "success" | "warning" | "danger"> = { default: "outline", primary: "default", success: "success", warning: "warning", danger: "danger" };

/** The look of an action button: the `variant` of the action, or the plain outline. */
export const actionLook = (a: Pick<api.ActionInfo, "variant">) => VARIANT_LOOK[a.variant ?? "default"];

/** The values of the form fields that a record loaded: a delete sends them back, so the server can see a stale record. */
export function loadedValues(meta: api.Resource, fields: { label: string }[], values: unknown[]): api.Values {
  const loaded: api.Values = {};
  for (const f of meta.form ?? []) {
    const at = fields.findIndex((c) => c.label === f.name);
    if (at >= 0) loaded[f.name] = values[at] as api.Values[string];
  }
  return loaded;
}

const mayDo = (meta: api.Resource, op: "delete" | "update") => meta.writable !== false && api.may(meta, op);
const recordAllows = (meta: api.Resource, flags: api.Flags, rule: string) => !(meta.can ?? []).includes(rule) || flags[rule] === true;

/** Delete (danger), with its question. A `can` rule that the record does not allow turns it off. It renders nothing when the user may not delete. */
export function DeleteButton({ meta, id, flags, loaded }: { meta: api.Resource; id: string; flags: api.Flags; loaded: api.Values }) {
  const client = useQueryClient();
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string[]>([]);
  const [ins, setIns] = useState<InputValues>(() => startInputs(meta.inputs));
  const [bad, setBad] = useState<Record<string, string[]>>({});
  if (!mayDo(meta, "delete")) return null;
  const ok = recordAllows(meta, flags, "delete");
  const remove = async () => {
    setBusy(true);
    setProblem([]);
    setBad({});
    try {
      await api.destroy(meta.name, id, loaded, inputBody(meta.inputs, ins));
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      client.removeQueries({ queryKey: ["detail", meta.name, id] });
      client.removeQueries({ queryKey: ["form", meta.name, id] });
      navigate(listBack(meta.name));
    } catch (e) {
      const body = e instanceof api.ApiError ? e.body : {};
      const stale = Object.keys(body.conflicts ?? {}).length > 0;
      setBad(body.fields ?? {});
      setProblem(stale ? ["This record changed after you opened it. Reload the page and check it before you delete it."] : body.form?.length ? body.form : [message(e)]);
      setBusy(false);
    }
  };
  return (
    <>
      <Button variant="danger" disabled={!ok || asking} title={ok ? undefined : "This record does not allow delete"} onClick={() => setAsking(true)}>Delete</Button>
      {asking && (
        <ConfirmDialog
          title="Confirm delete"
          description={`Delete this ${meta.title} for good? This cannot be undone.`}
          confirm="Delete it"
          destructive
          busy={busy}
          onConfirm={remove}
          onClose={() => { setAsking(false); setProblem([]); }}
        >
          <div className="grid gap-3">
            {(meta.inputs?.length ?? 0) > 0 && <InputFields defs={meta.inputs ?? []} vals={ins} errors={bad} onChange={(n, v) => setIns((s) => ({ ...s, [n]: v }))} scope="delete" />}
            {problem.map((m) => <Alert key={m}>{m}</Alert>)}
          </div>
        </ConfirmDialog>
      )}
    </>
  );
}

/** The footer of a detail page: Delete on the left, Edit on the right. It renders nothing when the user may do neither. */
export function DetailFooter({ meta, id, flags, loaded }: { meta: api.Resource; id: string; flags: api.Flags; loaded: api.Values }) {
  const canEdit = mayDo(meta, "update");
  if (!canEdit && !mayDo(meta, "delete")) return null;
  return (
    <StickyFooter left={<DeleteButton meta={meta} id={id} flags={flags} loaded={loaded} />}>
      {canEdit &&
        (recordAllows(meta, flags, "change") ? (
          <Link className={buttonVariants({ variant: "default" })} href={editUrl(meta.name, id)}>Edit</Link>
        ) : (
          <Button variant="outline" disabled title="This record does not allow change">Edit</Button>
        ))}
    </StickyFooter>
  );
}

/** The icon of an action, if it has one, and its label. */
export function ActionFace({ a }: { a: Pick<api.ActionInfo, "icon" | "label"> }) {
  return <>{a.icon && <NavIcon name={a.icon} className="size-4" />}{a.label}</>;
}

/**
 * The header bar of a detail page: a primary `+` when the user may add, the `detail` actions that have no menu as buttons, one dropdown for each `menu`, and "More" (the buttons over three, then History).
 * The action dialog sits next to the bar.
 */
export function DetailHeaderActions({ meta, id, flags, detail, hasAudit }: { meta: api.Resource; id: string; flags: api.Flags; detail: api.Detail; hasAudit: boolean }) {
  const [pending, setPending] = useState<api.ActionInfo | null>(null);
  const quick = useQuick(meta.name, (action) => setPending(action));
  const target: Target = { keys: [id], what: "this record" };
  const writable = meta.writable !== false;
  const actions = (meta.actions ?? []).filter((a) => a.placement === "detail" && (writable || (a.name === "set_password" && detail.password_target)));
  const groups = groupActions(actions);
  const run = (a: api.ActionInfo) => (needsDialog(a) ? setPending(a) : void quick(a, target));
  const history = hasAudit ? `${listUrl("AdminLogEntry")}?f.resource=${encodeURIComponent(meta.name)}&f.record_key=${encodeURIComponent(decodeURIComponent(id))}` : null;
  const moreItems: ReactNode[] = [
    ...groups.more.map((a) => <ActionItem key={a.name} a={a} meta={meta} flags={flags} run={run} />),
    ...(history ? [<DropdownMenuItem key="history" onSelect={() => navigate(history)}><History aria-hidden="true" className="size-4" />History</DropdownMenuItem>] : []),
  ];
  const menus = groups.menus.map((m) => (
    <DropdownMenu key={m.label}>
      <DropdownMenuTrigger asChild><Button variant="outline">{m.label}</Button></DropdownMenuTrigger>
      <DropdownMenuContent align="end">{m.items.map((a) => <ActionItem key={a.name} a={a} meta={meta} flags={flags} run={run} />)}</DropdownMenuContent>
    </DropdownMenu>
  ));
  return (
    <>
      <div className="actions flex flex-wrap items-center gap-2" role="group" aria-label="Actions">
        {writable && api.may(meta, "create") && <Link className={buttonVariants({ variant: "default" })} href={newUrl(meta.name)} title={`Add ${meta.title}`}><Plus aria-hidden="true" />Add</Link>}
        {groups.buttons.map((a) => {
          const reason = actionDisabled(a, meta.can ?? [], flags);
          return <Button key={a.name} variant={actionLook(a)} disabled={reason !== undefined} title={reason} onClick={() => run(a)}><ActionFace a={a} /></Button>;
        })}
        {menus}
        {moreItems.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild><Button variant="outline"><Ellipsis aria-hidden="true" />More</Button></DropdownMenuTrigger>
            <DropdownMenuContent align="end">{moreItems}</DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {pending && <ActionDialog key={pending.name} resource={meta.name} action={pending} target={target} passwordTarget={detail.password_target} onClose={() => setPending(null)} />}
    </>
  );
}

function ActionItem({ a, meta, flags, run }: { a: api.ActionInfo; meta: api.Resource; flags: api.Flags; run: (a: api.ActionInfo) => void }) {
  const reason = actionDisabled(a, meta.can ?? [], flags);
  return <DropdownMenuItem disabled={reason !== undefined} title={reason} onSelect={() => run(a)}><ActionFace a={a} /></DropdownMenuItem>;
}
