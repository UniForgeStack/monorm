// The native artifact's capabilities and extension ids.
import { resolveExtensions } from "../src/packages.js";
import assert from "node:assert/strict";
import { native } from "../src/native.js";

const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[]; extensions: Record<string, string> };

export function artifactCapabilities(): string[] {
  return artifact.capabilities;
}

/** True when the native artifact contains every named extension, such as `orm.locking`. */
export function hasExtension(...ids: string[]): boolean {
  const installed = { ...artifact.extensions, ...resolveExtensions() };
  for (const id of ids) if (["orm.timestamps", "orm.soft-delete", "orm.locking", "orm.inherit"].includes(id))
    assert.ok(id in installed, `install ${id} with scripts/install-extension-packages.sh`);
  return ids.every((id) => id in installed);
}
