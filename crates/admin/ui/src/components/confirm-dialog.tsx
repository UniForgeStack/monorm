import type { ReactNode } from "react";
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "./ui/alert-dialog";
import { Button } from "./ui/button";

/**
 * A question that needs an answer before the page goes on. It is open while it is mounted; the owner unmounts it on `onClose`.
 * `title` is its accessible name, `children` the body (a form may sit in it), `confirm` the button that goes on.
 */
export function ConfirmDialog({ title, description, children, confirm, busy = false, destructive = false, onConfirm, onClose }: {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  confirm: string;
  busy?: boolean;
  destructive?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <AlertDialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description ?? "Choose to go on or to cancel."}</AlertDialogDescription>
        </AlertDialogHeader>
        {children}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
          <Button variant={destructive ? "destructive" : "default"} disabled={busy} onClick={onConfirm}>{confirm}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
