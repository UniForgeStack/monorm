// The native artifact's capabilities and extension ids.
import { resolveExtensions } from "../src/packages.js";
import assert from "node:assert/strict";
import { native } from "../src/native.js";

const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[]; extensions: Record<string, string> };

export function artifactCapabilities(): string[] {
  return artifact.capabilities;
}

/**
 * True when the native artifact or an installed package contains every named extension, such as `orm.file-storage`.
 * A missing `orm.timestamps`, `orm.soft-delete`, `orm.locking`, `orm.inherit`, `orm.query-defaults` or `orm.generic` package fails with an assert.
 */
export function hasExtension(...ids: string[]): boolean {
  const installed = { ...artifact.extensions, ...resolveExtensions() };
  for (const id of ids) if (["orm.timestamps", "orm.soft-delete", "orm.locking", "orm.inherit", "orm.query-defaults", "orm.generic"].includes(id))
    assert.ok(id in installed, `install ${id} with scripts/install-extension-packages.sh`);
  return ids.every((id) => id in installed);
}
