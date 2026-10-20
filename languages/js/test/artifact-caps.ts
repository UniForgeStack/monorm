// The native artifact's capabilities and extension ids.
import { native } from "../src/native.js";

const artifact = JSON.parse(native().nativeArtifact()) as { capabilities: string[]; extensions: Record<string, string> };

export function artifactCapabilities(): string[] {
  return artifact.capabilities;
}

/** True when the native artifact contains every named extension, such as `orm.locking`. */
export function hasExtension(...ids: string[]): boolean {
  return ids.every((id) => id in artifact.extensions);
}
