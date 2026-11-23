import { useEffect, useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { LayoutDashboard } from "lucide-react";
import { EmbedPage, embedSite } from "./Embed";
import { Shell } from "./Shell";
import * as api from "./api";
import { buttonVariants } from "./components/ui/button";
import { Toaster } from "./components/ui/toast";
import { BASE, LOGIN_URL, navigate, parse, usePath } from "./router";
import { Alert, Button, Card, Field, message } from "./ui";

export function App() {
  return embedSite === null ? <FullAdmin /> : <EmbedPage siteName={embedSite} />;
}

function FullAdmin() {
  const path = usePath();
  const route = parse(path);
  const client = useQueryClient();
  const state = useQuery({ queryKey: ["session"], queryFn: api.session, retry: false, staleTime: Infinity });

  useEffect(() => {
    if (state.data && !state.data.authenticated && route.page !== "login") navigate(LOGIN_URL, true);
    if (state.data?.authenticated && route.page === "login") navigate(BASE, true);
  }, [state.data, route.page]);

  if (state.isPending) return <p className="grid min-h-screen place-items-center text-muted-foreground">Loading…</p>;
  if (state.isError) return <div className="grid min-h-screen place-items-center p-4"><Alert>{message(state.error)}</Alert></div>;
  if (!state.data.authenticated) return <Login sso={state.data.sso} onDone={() => client.invalidateQueries({ queryKey: ["session"] })} />;
  return <Shell user={state.data.user} route={route} />;
}

// The callback sends the browser to `<base>/?sso_error=<word>`; the redirect to the login page drops the query, so read it once at load.
const SSO_ERRORS: Record<string, string> = {
  inactive: "This account is not active.",
  domain: "This account is not in the allowed domain.",
  unavailable: "The sign-in provider did not answer. Try again later.",
  denied: "The sign-in was cancelled.",
  too_many: "Too many sign-in attempts. Try again later.",
};
const SSO_ERROR = (() => {
  const word = new URLSearchParams(window.location.search).get("sso_error");
  return word === null ? null : (SSO_ERRORS[word] ?? "The sign-in failed.");
})();

function Login({ sso, onDone }: { sso: string | null; onDone: () => void }) {
  const [error, setError] = useState<string | null>(SSO_ERROR);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await api.login(String(form.get("username")), String(form.get("password")));
      navigate(BASE, true);
      onDone();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <main className="grid min-h-screen place-items-center p-4">
      <Toaster />
      <div className="grid w-full max-w-sm justify-items-center gap-4">
        <span className="grid size-12 place-items-center rounded-xl bg-primary text-primary-foreground"><LayoutDashboard aria-hidden /></span>
        <Card title="Sign in">
          <form onSubmit={submit}>
            <Field label="Username" name="username" autoComplete="username" required autoFocus />
            <Field label="Password" name="password" type="password" autoComplete="current-password" required />
            {error && <Alert>{error}</Alert>}
            <Button type="submit" disabled={busy} className="w-full">Sign in</Button>
          </form>
          {sso && <p className="mt-4"><a className={buttonVariants({ variant: "outline", className: "w-full" })} href={sso}>Sign in with single sign-on</a></p>}
        </Card>
      </div>
    </main>
  );
}
