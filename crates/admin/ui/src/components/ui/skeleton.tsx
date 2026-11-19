import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

/** A grey block that pulses while data loads. */
export const Skeleton = ({ className, ...props }: HTMLAttributes<HTMLDivElement>) => <div aria-hidden="true" className={cn("animate-pulse rounded-md bg-muted", className)} {...props} />;
