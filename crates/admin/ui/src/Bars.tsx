import { ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import { BASE, Link } from "./router";
import { cn } from "./lib/utils";

/** The trail above the title: the site, the group of the resource (text: a group has no page), the resource and the record. The last item is the page and not a link. */
export function Breadcrumbs({ group, items }: { group?: string | null; items: { label: string; href?: string }[] }) {
  const trail = [{ label: "Home", href: BASE }, ...(group ? [{ label: group, href: undefined }] : []), ...items];
  return (
    <nav aria-label="Breadcrumb" className="mb-2 text-sm text-muted-foreground">
      <ol className="m-0 flex list-none flex-wrap items-center gap-1 p-0">
        {trail.map((item, i) => (
          <li key={`${i}:${item.label}`} className="flex items-center gap-1">
            {i > 0 && <ChevronRight aria-hidden="true" className="size-3.5" />}
            {item.href && i < trail.length - 1 ? <Link href={item.href}>{item.label}</Link> : <span aria-current={i === trail.length - 1 ? "page" : undefined} className={cn(i === trail.length - 1 && "text-foreground")}>{item.label}</span>}
          </li>
        ))}
      </ol>
    </nav>
  );
}

/** The bar at the bottom of a record page. It sticks to the viewport: Delete (or nothing) on the left, the actions that go on from here on the right. */
export function StickyFooter({ left, children, className }: { left?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <div role="group" aria-label="Page actions" className={cn("savebar sticky bottom-0 z-10 mt-2 flex flex-wrap items-center gap-2 rounded-lg border-t border-border bg-card p-3 shadow-[0_-4px_12px_rgb(0_0_0/0.08)]", className)}>
      {left}
      <div className="ml-auto flex flex-wrap items-center gap-2">{children}</div>
    </div>
  );
}
