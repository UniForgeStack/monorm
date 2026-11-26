import { cva, type VariantProps } from "class-variance-authority";
import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import { cn } from "../lib/utils";

const bannerVariants = cva("flex items-start gap-2 rounded-md border px-3 py-2 text-sm", {
  variants: {
    level: {
      info: "border-info/40 bg-info-soft text-info",
      warning: "border-warning/40 bg-warning-soft text-warning",
      danger: "border-destructive/40 bg-danger-soft text-destructive",
    },
  },
  defaultVariants: { level: "info" },
});

export type BannerLevel = NonNullable<VariantProps<typeof bannerVariants>["level"]>;

const icons = { info: Info, warning: TriangleAlert, danger: CircleAlert } as const;

/** A notice about one record. A danger banner is an alert for a screen reader; the others are a status. */
export function Banner({ level, text, link }: { level: BannerLevel; text: string; link?: { label: string; href: string } }) {
  const Icon = icons[level];
  return (
    <div className={cn(bannerVariants({ level }))} role={level === "danger" ? "alert" : "status"} data-banner={level}>
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <span className="min-w-0 break-words">{text}</span>
      {link && <a className="ml-auto shrink-0 underline" href={link.href}>{link.label}</a>}
    </div>
  );
}

export function Banners({ banners }: { banners: { level: BannerLevel; text: string }[] | undefined }) {
  if (!banners || banners.length === 0) return null;
  return (
    <div className="my-3 flex flex-col gap-2" data-testid="banners">
      {banners.map((b, i) => <Banner key={i} level={b.level} text={b.text} />)}
    </div>
  );
}
