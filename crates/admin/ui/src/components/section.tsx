import { useState, type ReactNode } from "react";
import { readOpen, writeOpen } from "../lib/fold";
import { Collapsible } from "./ui/collapsible";

/** The card look of a section: the element differs (a div, a details or a fieldset), so the look sits in one constant. */
export const sectionCard = "mb-4 rounded-lg border border-border bg-card p-5 text-card-foreground shadow-xs";
export const sectionTitle = "text-xs font-semibold tracking-wide uppercase text-muted-foreground";

/** A card with an uppercase title that folds. The state is kept for each resource and section; `collapsed` is the state before the user chose. With no `resource` (a read-only embed) the card does not fold. */
export function Section({ resource, name, title, collapsed = false, id, children }: { resource?: string; name: string; title: string; collapsed?: boolean; id?: string; children: ReactNode }) {
  const [open, setOpen] = useState(() => (resource === undefined ? true : readOpen(resource, name, collapsed)));
  if (resource === undefined) {
    return (
      <section aria-labelledby={id} className={sectionCard}>
        <h2 id={id} className={`m-0 mb-4 ${sectionTitle}`}>{title}</h2>
        {children}
      </section>
    );
  }
  return (
    <div role="region" aria-labelledby={id} className={sectionCard}>
      <Collapsible
        heading
        title={<span id={id}>{title}</span>}
        titleClassName={sectionTitle}
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
