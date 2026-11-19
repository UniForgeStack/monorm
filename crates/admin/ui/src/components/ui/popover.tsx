import * as PopoverPrimitive from "@radix-ui/react-popover";
import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

/** A non-modal panel under its trigger. Radix puts the focus in the panel on open, returns it to the trigger on Escape, and flips the panel at the edge of the window. */
export function Popover({ trigger, children, label, className }: { trigger: ReactNode; children: ReactNode; label: string; className?: string }) {
  return (
    <PopoverPrimitive.Root>
      <PopoverPrimitive.Trigger asChild>{trigger}</PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          role="group"
          aria-label={label}
          align="start"
          sideOffset={4}
          collisionPadding={8}
          className={cn("z-40 max-h-[min(70vh,var(--radix-popover-content-available-height))] w-[min(34rem,90vw)] overflow-auto rounded-md border border-border bg-popover p-4 text-popover-foreground shadow-md outline-none", className)}
        >
          {children}
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
