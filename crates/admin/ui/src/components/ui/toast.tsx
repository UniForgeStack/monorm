import { CircleAlert, CircleCheck, Info, TriangleAlert, X } from "lucide-react";
import { useSyncExternalStore } from "react";
import { cn } from "../../lib/utils";

export type ToastLevel = "info" | "success" | "warning" | "error";
interface Item {
  id: number;
  text: string;
  level: ToastLevel;
}

let items: Item[] = [];
let next = 1;
const watchers = new Set<() => void>();
const publish = (list: Item[]) => {
  items = list;
  watchers.forEach((fn) => fn());
};
const dismiss = (id: number) => publish(items.filter((t) => t.id !== id));

/** Show a message. It leaves by itself after eight seconds. The store is the page's own: no style or script leaves the bundle. */
export function toast(text: string, level: ToastLevel = "info") {
  const id = next++;
  publish([...items, { id, text, level }]);
  setTimeout(() => dismiss(id), 8000);
}

const icons = { info: Info, success: CircleCheck, warning: TriangleAlert, error: CircleAlert };
const tone: Record<ToastLevel, string> = {
  info: "text-foreground",
  success: "text-success",
  warning: "text-warning",
  error: "text-destructive",
};

/** The messages of the page: render it once, at the root. */
export function Toaster() {
  const list = useSyncExternalStore(
    (fn) => (watchers.add(fn), () => void watchers.delete(fn)),
    () => items,
  );
  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-[60] flex w-[min(24rem,calc(100vw-2rem))] flex-col gap-2" aria-live="polite">
      {list.map((t) => {
        const Icon = icons[t.level];
        return (
          <div key={t.id} className="pointer-events-auto flex items-start gap-3 rounded-lg border border-border bg-popover p-3 text-sm text-popover-foreground shadow-lg" role={t.level === "error" ? "alert" : "status"}>
            <Icon className={cn("mt-0.5 size-4 shrink-0", tone[t.level])} aria-hidden />
            <span className="flex-1">{t.text}</span>
            <button type="button" className="rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
              <X className="size-4" aria-hidden />
            </button>
          </div>
        );
      })}
    </div>
  );
}
