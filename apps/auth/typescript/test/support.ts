/** The test harness: models loaded from a consumer schema that imports the app. */

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Database, Registry, connect, load, useModelMethods, useQuerySet } from "orm";

import { UserMethods, UserQueries } from "../src/index.js";
import type { AnyModel } from "../src/permissions.js";

export type Rows = Record<string, any>;

// the package root holds `package.json`; this file is in `test/` or in `dist/test/`
let root = dirname(fileURLToPath(import.meta.url));
while (!existsSync(join(root, "package.json"))) root = dirname(root);
export const PACKAGE = root;

export const VECTORS = JSON.parse(readFileSync(join(PACKAGE, "../fixtures/hashes.json"), "utf8")) as {
  pbkdf2: { raw: string; encoded: string; valid: boolean }[];
  argon2id: { raw: string; encoded: string; valid: boolean; needs_update: boolean }[];
};

const POSTGRES_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const HEADER = (provider: string): string => `datasource db {\n  provider = "${provider}"\n}\nimport "app:orm-auth" (prefix: "auth_")\n`;

export const PROVIDERS = ["sqlite", "postgres"] as const;
export type Provider = (typeof PROVIDERS)[number];

/** A consumer schema: the `schema` text that adds a user model, its `model` name, and the `base` name of the join models. */
export interface Variant {
  readonly schema: string;
  readonly model: string;
  readonly base: string;
  readonly extends: boolean;
}

// an override keeps the join models of `User`; an `extends` child gets its own (`AccountRole`, `accountId`)
export const VARIANTS: Readonly<Record<string, Variant>> = {
  plain: { schema: "", model: "User", base: "User", extends: false },
  override: { schema: 'model MyUser {\n  bio String?\n  @@inherit.override("User")\n}\n', model: "MyUser", base: "User", extends: false },
  extends: { schema: 'model Account {\n  bio String?\n  @@inherit.extends("AbstractUser")\n}\n', model: "Account", base: "Account", extends: true },
};

export class Auth {
  readonly User: Rows;
  readonly Role: Rows;
  readonly Permission: Rows;
  readonly UserRole: Rows;
  readonly UserPermission: Rows;
  readonly userId: string;

  constructor(
    readonly db: Database,
    readonly registry: Registry,
    readonly models: Rows,
    readonly variant: Variant,
    readonly dir: string,
  ) {
    this.User = models[variant.model] as Rows;
    this.Role = models["Role"] as Rows;
    this.Permission = models["Permission"] as Rows;
    this.UserRole = models[`${variant.base}Role`] as Rows;
    this.UserPermission = models[`${variant.base}Permission`] as Rows;
    this.userId = `${variant.base.charAt(0).toLowerCase()}${variant.base.slice(1)}Id`;
  }

  /** The options of `authenticate`: the registry cannot find an `extends` child, so the model is passed. */
  get options(): { using: Database; registry: Registry; model?: AnyModel } {
    return this.variant.extends ? { using: this.db, registry: this.registry, model: this.User as never } : { using: this.db, registry: this.registry };
  }

  async close(): Promise<void> {
    await this.db.dropTables();
    await this.db.close();
    rmSync(this.dir, { recursive: true, force: true });
  }

  /** Users `staff` (a role with add_post), `direct` (change_post), `root` (superuser), `idle` (inactive, has the role). */
  async seed(): Promise<Record<"staff" | "direct" | "root" | "idle", Rows>> {
    const { db, models: m } = this;
    const permissions: Rows[] = await this.Permission.objects
      .using(db)
      .insertMany([
        { label: "blog", codename: "add_post", name: "Can add post" },
        { label: "blog", codename: "change_post", name: "Can change post" },
        { label: "shop", codename: "add_post", name: "Can add post" },
      ])
      .returning();
    const [role] = (await this.Role.objects.using(db).insertMany([{ name: "editor" }]).returning()) as Rows[];
    await m["RolePermission"].objects.using(db).insertMany([{ roleId: role!.id, permissionId: permissions[0]!.id }]);
    const [staff, direct, root, idle] = (await this.User.objects
      .using(db)
      .insertMany([
        { username: "staff", email: "s@x.y", password: "!" },
        { username: "direct", email: "d@x.y", password: "!" },
        { username: "root", email: "r@x.y", password: "!", isSuperuser: true },
        { username: "idle", email: "i@x.y", password: "!", isActive: false, isSuperuser: true },
      ])
      .returning()) as Rows[];
    const uid = this.userId;
    await this.UserRole.objects.using(db).insertMany([
      { [uid]: staff!.id, roleId: role!.id },
      { [uid]: idle!.id, roleId: role!.id },
    ]);
    await this.UserPermission.objects.using(db).insertMany([{ [uid]: direct!.id, permissionId: permissions[1]!.id }]);
    return { staff: staff!, direct: direct!, root: root!, idle: idle! };
  }

  /** The stored row of the user `username`. */
  async stored(username: string): Promise<Rows> {
    return (await this.User.objects.using(this.db).get(this.User.username.eq(username))) as Rows;
  }
}

/** A connected `Auth` on empty tables, or `undefined` when the default Postgres is not reachable. */
export async function makeAuth(provider: Provider, variant: Variant): Promise<Auth | undefined> {
  const dir = mkdtempSync(join(tmpdir(), "orm-auth-"));
  mkdirSync(join(dir, "node_modules"));
  symlinkSync(PACKAGE, join(dir, "node_modules", "orm-auth"));
  // A fresh consumer declares pass packages directly, as package discovery requires.
  mkdirSync(join(dir, "node_modules", "@orm"));
  const dependencies: Record<string, string> = { "orm-auth": "0.1.0" };
  for (const name of ["timestamps", "soft-delete", "locking", "inherit"]) {
    symlinkSync(join(PACKAGE, "node_modules", "@orm", name), join(dir, "node_modules", "@orm", name));
    dependencies[`@orm/${name}`] = "0.1.0";
  }
  writeFileSync(join(dir, "package.json"), JSON.stringify({ dependencies }));
  const schema = join(dir, "schema.prisma");
  writeFileSync(schema, HEADER(provider) + variant.schema);
  const registry = new Registry();
  const models = load(schema, { registry }) as unknown as Rows;
  const user = models[variant.model];
  useModelMethods(user, UserMethods);
  useQuerySet(user, UserQueries);
  let db: Database;
  try {
    db = await connect(provider === "postgres" ? POSTGRES_URL : "sqlite://:memory:", { registry, default: false });
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
    return undefined;
  }
  await db.dropTables();
  await db.createTables();
  return new Auth(db, registry, models, variant, dir);
}
