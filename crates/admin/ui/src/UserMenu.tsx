import { ChevronsUpDown, KeyRound, LogOut, Monitor, Moon, Sun } from "lucide-react";
import * as api from "./api";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "./components/ui/dropdown-menu";
import type { Zone } from "./format";
import { setMode, useMode, type Mode } from "./lib/theme";
import { PASSWORD_URL, navigate } from "./router";
import { initials } from "./lib/ramp";

const MODES: { mode: Mode; label: string; icon: typeof Sun }[] = [
  { mode: "light", label: "Light", icon: Sun },
  { mode: "dark", label: "Dark", icon: Moon },
  { mode: "system", label: "System", icon: Monitor },
];

/** The avatar, the name and the address of the user at the foot of the sidebar; it opens the account menu. */
export function UserFooter({ user, zone, onZone, onSignOut }: { user: api.User | null; zone: Zone; onZone: (z: Zone) => void; onSignOut: () => void }) {
  const mode = useMode();
  const label = user?.display ?? user?.username ?? "Account";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button" aria-label="User menu" className="flex w-full items-center gap-3 rounded-md border-0 bg-transparent p-2 text-left hover:bg-accent">
          <span className="grid size-8 shrink-0 place-items-center rounded-full bg-primary text-xs font-semibold text-primary-foreground" aria-hidden>{initials(label)}</span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm font-medium text-foreground">{label}</span>
            {user?.username && user.username !== label && <span className="block truncate text-xs text-muted-foreground">{user.username}</span>}
          </span>
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="w-60">
        <DropdownMenuLabel>Theme</DropdownMenuLabel>
        <DropdownMenuRadioGroup value={mode} onValueChange={(v) => setMode(v as Mode)}>
          {MODES.map(({ mode: m, label: l, icon: Icon }) => (
            <DropdownMenuRadioItem key={m} value={m}>
              <Icon className="size-4" aria-hidden />
              {l}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuLabel>Time zone</DropdownMenuLabel>
        <DropdownMenuRadioGroup value={zone} onValueChange={(v) => onZone(v as Zone)}>
          <DropdownMenuRadioItem value="local">Browser time zone</DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="utc">UTC</DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => navigate(PASSWORD_URL)}>
          <KeyRound className="size-4" aria-hidden />
          Change password
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={onSignOut}>
          <LogOut className="size-4" aria-hidden />
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
