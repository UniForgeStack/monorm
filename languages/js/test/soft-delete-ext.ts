// Installed packages supply the same members to loads() as generated models.
import { loads, type Registry } from "../src/index.js";
export const ID = "orm.soft-delete";
export async function loadsSoftDelete(source: string, options: { registry: Registry }): Promise<ReturnType<typeof loads>> {
  return loads(source, options);
}
