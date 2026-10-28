// The native artifact's capabilities and the installed extension ids.
import { resolveExtensions } from "../src/packages.js";
import assert from "node:assert/strict";
import { native } from "../src/native.js";

const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[] };

export function artifactCapabilities(): string[] {
  return artifact.capabilities;
}

/**
 * True when an installed package contains every named extension, such as `monorm.file-storage`.
 * A missing `monorm.timestamps`, `monorm.soft-delete`, `monorm.locking`, `monorm.inherit`, `monorm.query-defaults`, `monorm.generic`, `monorm.proxy`, `monorm.composition` or `monorm.file-storage` package fails with an assert.
 */
export function hasExtension(...ids: string[]): boolean {
  const installed = resolveExtensions();
  for (const id of ids) if (["monorm.timestamps", "monorm.soft-delete", "monorm.locking", "monorm.inherit", "monorm.query-defaults", "monorm.generic", "monorm.proxy", "monorm.composition", "monorm.file-storage"].includes(id))
    assert.ok(id in installed, `install ${id} with scripts/install-extension-packages.sh`);
  return ids.every((id) => id in installed);
}
