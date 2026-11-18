import * as Menu from "@radix-ui/react-dropdown-menu";
import { Check } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "../../lib/utils";

// Not modal: a modal menu locks the page scroll with an injected <style> element, which the CSP (`style-src 'self'`) blocks.
export const DropdownMenu = (props: ComponentProps<typeof Menu.Root>) => <Menu.Root modal={false} {...props} />;
export const DropdownMenuTrigger = Menu.Trigger;
export const DropdownMenuRadioGroup = Menu.RadioGroup;

const item = "relative flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none select-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[disabled]:opacity-50";

export function DropdownMenuContent({ className, sideOffset = 4, ...props }: ComponentProps<typeof Menu.Content>) {
  return (
    <Menu.Portal>
      <Menu.Content sideOffset={sideOffset} className={cn("z-50 min-w-52 rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md", className)} {...props} />
    </Menu.Portal>
  );
}
export const DropdownMenuItem = ({ className, ...props }: ComponentProps<typeof Menu.Item>) => <Menu.Item className={cn(item, className)} {...props} />;
export const DropdownMenuLabel = ({ className, ...props }: ComponentProps<typeof Menu.Label>) => (
  <Menu.Label className={cn("px-2 py-1.5 text-xs font-medium text-muted-foreground", className)} {...props} />
);
export const DropdownMenuSeparator = ({ className, ...props }: ComponentProps<typeof Menu.Separator>) => (
  <Menu.Separator className={cn("-mx-1 my-1 h-px bg-border", className)} {...props} />
);
export function DropdownMenuRadioItem({ className, children, ...props }: ComponentProps<typeof Menu.RadioItem>) {
  return (
    <Menu.RadioItem className={cn(item, "pl-8", className)} {...props}>
      <span className="absolute left-2 flex size-4 items-center justify-center">
        <Menu.ItemIndicator>
          <Check className="size-4" aria-hidden />
        </Menu.ItemIndicator>
      </span>
      {children}
    </Menu.RadioItem>
  );
}
