// The native artifact's capabilities, plus a name for each tracking extension it contains.
import { native } from "../src/native.js";

const tracking: Record<string, string> = { "orm.timestamps": "updated-at", "orm.soft-delete": "soft-delete", "orm.locking": "optimistic-locking" };

export function artifactCapabilities(): string[] {
  const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[]; extensions: Record<string, string> };
  return [...artifact.capabilities, ...Object.entries(tracking).filter(([id]) => id in artifact.extensions).map(([, name]) => name)];
}
