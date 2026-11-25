import { useState, type ReactNode } from "react";
import { readOpen, writeOpen } from "../lib/fold";
import { Collapsible } from "./ui/collapsible";

const card = "mb-4 rounded-xl border border-border bg-card p-5 text-card-foreground shadow-xs";
const titleClass = "text-xs font-semibold tracking-wide uppercase text-muted-foreground";

/** A card with an uppercase title that folds. The state is kept for each resource and section; `collapsed` is the state before the user chose. With no `resource` (a read-only embed) the card does not fold. */
export function Section({ resource, name, title, collapsed = false, id, children }: { resource?: string; name: string; title: string; collapsed?: boolean; id?: string; children: ReactNode }) {
  const [open, setOpen] = useState(() => (resource === undefined ? true : readOpen(resource, name, collapsed)));
  if (resource === undefined) {
    return (
      <section aria-labelledby={id} className={card}>
        <h2 id={id} className={`m-0 mb-4 ${titleClass}`}>{title}</h2>
        {children}
      </section>
    );
  }
  return (
    <div role="region" aria-labelledby={id} className={card}>
      <Collapsible
        heading
        title={<span id={id}>{title}</span>}
        titleClassName={titleClass}
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          writeOpen(resource, name, next);
        }}
      >
        {children}
      </Collapsible>
    </div>
  );
}
