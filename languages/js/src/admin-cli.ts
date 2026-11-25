/**
 * `npx monorm admin`: serves the admin of this project.
 *
 * The settings come from the `[admin]` section (see `monorm_core::settings`). The `app` setting names the
 * `Admin` object (`./admin.ts:admin`). The `setup` setting names a function (`./setup.ts:setup`, sync or async)
 * that connects the database, the extensions and the file storage as the application does. Without `setup`
 * the admin connects to `MONORM_DATABASE_URL` after it imports `app`.
 */

import { AdminConfigError, AdminSettingsError, type Admin } from "./admin.js";
import { loadPasswordRules } from "./admin-host.js";
import { connect, getDatabase, type Database } from "./db.js";
import { NotConnected } from "./errors.js";
import { loadSpec as load } from "./load-spec.js";
import { native } from "./native.js";

interface Settings {
  readonly app: string;
  readonly setup: string | null;
  readonly base_path?: string | null;
  readonly switcher?: unknown;
}

async function open(settings: Settings): Promise<{ admin: Admin; database: Database }> {
  const admin = (await load(settings.app, "admin.app")) as Admin;
  if (typeof admin?.serve !== "function") throw new Error(`admin.app \`${settings.app}\` is not a monorm/admin Admin`);
  if (settings.setup !== null) await (await load(settings.setup, "admin.setup") as () => unknown)();
  // the deploy knows the gateway path: the setting wins over the option of the object
  if (settings.base_path) admin.basePath = settings.base_path;
  if (settings.switcher) admin.switcherSettings = settings.switcher;
  let database;
  try {
    database = getDatabase();
  } catch (e) {
    if (!(e instanceof NotConnected)) throw e;
    const url = process.env.MONORM_DATABASE_URL;
    if (!url) throw new Error("no database: the setup hook did not connect, and MONORM_DATABASE_URL is not set");
    database = await connect(url);
  }
  return { admin, database };
}

async function sync(settings: Settings): Promise<number> {
  const { admin, database } = await open(settings);
  try {
    let synced, seeded;
    try {
      synced = await admin.syncPermissions(database);
      seeded = await admin.seedRoles(database);
    } catch (e) {
      if (e instanceof AdminConfigError) throw new AdminSettingsError(e.message, e.code.startsWith("settings.") ? e.code : "settings.invalid_value");
      throw e;
    }
    process.stdout.write(`permissions: ${synced.created} created, ${synced.stale.length} stale\n`);
    for (const name of synced.stale) process.stdout.write(`stale: ${name}\n`);
    process.stdout.write(`roles: ${seeded.rolesCreated.length} created, ${seeded.grants} grants added\n`);
  } finally {
    await database.close();
  }
  return 0;
}

async function serve(settingsJson: string, settings: Settings): Promise<number> {
  await loadPasswordRules().catch((e) => {
    throw new AdminSettingsError(e instanceof Error ? e.message : String(e));
  });
  const { admin, database } = await open(settings);
  try {
    const server = await admin.serve(database, settingsJson, process.env.MONORM_ADMIN_SECRET ?? null);
    const stop = () => void server.stop().catch(() => undefined);
    process.once("SIGTERM", stop);
    process.once("SIGINT", stop);
    await server.wait();
  } finally {
    await database.close();
  }
  return 0;
}

/** Gives the exit code: 0 after a clean stop, 2 for wrong settings, 1 for any other failure. `command` is `serve` or `sync-permissions`. */
export async function run(command: "serve" | "sync-permissions" = "serve"): Promise<number> {
  let settingsJson: string;
  try {
    settingsJson = native().adminSettings(process.cwd());
  } catch (e) {
    process.stderr.write(`error: ${e instanceof Error ? e.message : String(e)}\n`);
    return 2;
  }
  try {
    const settings = JSON.parse(settingsJson) as Settings;
    return command === "sync-permissions" ? await sync(settings) : await serve(settingsJson, settings);
  } catch (e) {
    process.stderr.write(`error: ${e instanceof Error ? e.message : String(e)}\n`);
    return e instanceof AdminSettingsError ? 2 : 1;
  }
}
