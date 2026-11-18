// A modal on the native <dialog> element. Radix modals inject a <style> element for the scroll lock, which the CSP (`style-src 'self'`) blocks.
// The browser gives the focus trap, the inert page and the Escape key; the page scroll lock is `body:has(dialog[open])` in styles.css.
import { Slot } from "@radix-ui/react-slot";
import { X } from "lucide-react";
import { createContext, useContext, useEffect, useId, useRef, useState, type ComponentProps, type HTMLAttributes, type MouseEvent, type ReactNode } from "react";
import { cn } from "../../lib/utils";

type Modal = { open: boolean; setOpen: (open: boolean) => void; titleId: string; descriptionId: string };
const ModalContext = createContext<Modal | null>(null);

function useModal(): Modal {
  const modal = useContext(ModalContext);
  if (!modal) throw new Error("a dialog part must be inside <Dialog>");
  return modal;
}

export function Dialog({ open, defaultOpen = false, onOpenChange, children }: { open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; children?: ReactNode }) {
  const [inner, setInner] = useState(defaultOpen);
  const id = useId();
  const shown = open ?? inner;
  const setOpen = (next: boolean) => {
    if (open === undefined) setInner(next);
    onOpenChange?.(next);
  };
  return <ModalContext.Provider value={{ open: shown, setOpen, titleId: `${id}-title`, descriptionId: `${id}-description` }}>{children}</ModalContext.Provider>;
}

export function DialogTrigger({ asChild, onClick, ...props }: ComponentProps<"button"> & { asChild?: boolean }) {
  const { setOpen } = useModal();
  const Component = asChild ? Slot : "button";
  return <Component type={asChild ? undefined : "button"} aria-haspopup="dialog" onClick={(e: MouseEvent<HTMLButtonElement>) => { onClick?.(e); if (!e.defaultPrevented) setOpen(true); }} {...props} />;
}

export function DialogClose({ asChild, onClick, ...props }: ComponentProps<"button"> & { asChild?: boolean }) {
  const { setOpen } = useModal();
  const Component = asChild ? Slot : "button";
  return <Component type={asChild ? undefined : "button"} onClick={(e: MouseEvent<HTMLButtonElement>) => { onClick?.(e); if (!e.defaultPrevented) setOpen(false); }} {...props} />;
}

/** `role="alertdialog"` is for a question that needs an answer: a click on the backdrop does not close it. */
export function ModalContent({ className, children, role = "dialog", closeButton = true, onCancel, onClick, ...props }: ComponentProps<"dialog"> & { closeButton?: boolean }) {
  const { open, setOpen, titleId, descriptionId } = useModal();
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!open || !dialog) return;
    if (!dialog.open) dialog.showModal();
    return () => dialog.close();
  }, [open]);
  if (!open) return null;
  return (
    <dialog
      ref={ref}
      role={role}
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
      {...props}
      className={cn("m-auto max-h-[90vh] w-[min(32rem,92vw)] overflow-auto rounded-xl border border-border bg-popover p-0 text-popover-foreground shadow-lg backdrop:bg-black/50", className)}
      // The caller handlers run first; the close logic is last and always runs, so a caller cannot replace it.
      onCancel={(e) => { onCancel?.(e); e.preventDefault(); setOpen(false); }}
      onClick={(e) => { onClick?.(e); if (role === "dialog" && e.target === e.currentTarget) setOpen(false); }}
    >
      <div className="relative grid gap-4 p-6">
        {children}
        {closeButton && (
          <DialogClose className="absolute top-3 right-3 rounded-sm p-1 text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" aria-label="Close">
            <X className="size-4" aria-hidden />
          </DialogClose>
        )}
      </div>
    </dialog>
  );
}

export const DialogContent = ModalContent;
export const DialogHeader = ({ className, ...props }: HTMLAttributes<HTMLDivElement>) => <div className={cn("flex flex-col gap-1.5", className)} {...props} />;
export const DialogFooter = ({ className, ...props }: HTMLAttributes<HTMLDivElement>) => <div className={cn("flex flex-wrap justify-end gap-2", className)} {...props} />;
export function DialogTitle({ className, ...props }: ComponentProps<"h2">) {
  return <h2 id={useModal().titleId} className={cn("m-0 text-lg leading-none font-semibold tracking-normal text-foreground normal-case", className)} {...props} />;
}
export function DialogDescription({ className, ...props }: ComponentProps<"div">) {
  return <div id={useModal().descriptionId} className={cn("m-0 text-sm text-muted-foreground", className)} {...props} />;
}
