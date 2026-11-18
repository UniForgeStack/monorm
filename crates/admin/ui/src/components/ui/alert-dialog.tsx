import type { ComponentProps } from "react";
import { cn } from "../../lib/utils";
import { buttonVariants, type ButtonProps } from "./button";
import { Dialog, DialogClose, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger, ModalContent } from "./dialog";

export const AlertDialog = Dialog;
export const AlertDialogTrigger = DialogTrigger;
export const AlertDialogHeader = DialogHeader;
export const AlertDialogFooter = DialogFooter;
export const AlertDialogTitle = DialogTitle;
export const AlertDialogDescription = DialogDescription;

export function AlertDialogContent({ className, ...props }: ComponentProps<"dialog">) {
  return <ModalContent role="alertdialog" closeButton={false} className={cn("w-[min(28rem,92vw)]", className)} {...props} />;
}
export function AlertDialogAction({ className, variant, size, ...props }: ComponentProps<"button"> & Pick<ButtonProps, "variant" | "size">) {
  return <DialogClose className={cn(buttonVariants({ variant, size }), className)} {...props} />;
}
export function AlertDialogCancel({ className, ...props }: ComponentProps<"button">) {
  return <DialogClose className={cn(buttonVariants({ variant: "outline" }), className)} {...props} />;
}
