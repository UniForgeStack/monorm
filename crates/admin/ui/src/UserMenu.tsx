import { ChevronDown, KeyRound, LogOut, Monitor, Moon, Sun } from "lucide-react";
import * as api from "./api";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "./components/ui/dropdown-menu";
import type { Zone } from "./format";
import { setMode, useMode, type Mode } from "./lib/theme";
import { PASSWORD_URL, navigate } from "./router";
import { Button } from "./ui";

const MODES: { mode: Mode; label: string; icon: typeof Sun }[] = [
  { mode: "light", label: "Light", icon: Sun },
  { mode: "dark", label: "Dark", icon: Moon },
  { mode: "system", label: "System", icon: Monitor },
];

export function UserMenu({ user, zone, onZone, onSignOut }: { user: api.User | null; zone: Zone; onZone: (z: Zone) => void; onSignOut: () => void }) {
  const mode = useMode();
  const label = user?.display ?? user?.username ?? "Account";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="plain" aria-label="User menu">
          {label}
          <ChevronDown aria-hidden />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
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
