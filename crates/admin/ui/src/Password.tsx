import { useState, type FormEvent } from "react";
import * as api from "./api";
import { Alert, Button, Card, Field, message } from "./ui";

export function Password() {
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = e.currentTarget;
    const data = new FormData(form);
    setError(null);
    setDone(false);
    if (data.get("new") !== data.get("again")) return setError("The two new passwords differ.");
    try {
      await api.changePassword(String(data.get("old")), String(data.get("new")));
      setDone(true);
      form.reset();
    } catch (err) {
      setError(message(err));
    }
  };
  return (
    <Card title="Change password">
      <form onSubmit={submit}>
        <Field label="Current password" name="old" type="password" autoComplete="current-password" required />
        <Field label="New password" name="new" type="password" autoComplete="new-password" required />
        <Field label="New password again" name="again" type="password" autoComplete="new-password" required />
        {error && <Alert>{error}</Alert>}
        {done && <Alert kind="ok">The password changed.</Alert>}
        <Button type="submit">Change password</Button>
      </form>
    </Card>
  );
}
