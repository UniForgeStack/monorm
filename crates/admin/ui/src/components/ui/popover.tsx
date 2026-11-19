import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { cn } from "../../lib/utils";

/** A non-modal panel under its trigger. It closes on Escape and on a click outside, and it adds no style element (the CSP is `style-src 'self'`). */
export function Popover({ trigger, children, label, className }: { trigger: (props: { open: boolean; toggle: () => void; id: string }) => ReactNode; children: ReactNode; label: string; className?: string }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const id = useId();
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => !root.current?.contains(e.target as Node) && setOpen(false);
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      setOpen(false);
      root.current?.querySelector<HTMLElement>("[aria-controls]")?.focus();
    };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", key);
    };
  }, [open]);
  return (
    <div ref={root} className="relative">
      {trigger({ open, toggle: () => setOpen((o) => !o), id })}
      {open && (
        <div id={id} role="group" aria-label={label} className={cn("absolute left-0 top-full z-40 mt-1 max-h-[70vh] w-[min(34rem,90vw)] overflow-auto rounded-md border border-border bg-popover p-4 text-popover-foreground shadow-md", className)}>
          {children}
        </div>
      )}
    </div>
  );
}
