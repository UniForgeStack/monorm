/** Password hashing: argon2id (PHC strings) and Django `pbkdf2_sha256$` hashes. */

import { hashSync, verifySync, type Algorithm } from "@node-rs/argon2";
import { pbkdf2Sync, timingSafeEqual } from "node:crypto";

// The same parameters as `hashers.py`, so a hash from one language does not need an update in the other.
const MEMORY_KIB = 65536;
const TIME_COST = 3;
const PARALLELISM = 4;
// `Algorithm.Argon2id`: the enum is `const` in the typings, which a per-file transpiler cannot inline.
const ARGON2ID = 2 as Algorithm;

const PARAMS = /^\$argon2id\$v=19\$m=(\d+),t=(\d+),p=(\d+)\$/;
const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

/** Hashes `raw` with argon2id and a new salt; the result is a PHC string. */
export function makePassword(raw: string): string {
  return hashSync(raw, { algorithm: ARGON2ID, memoryCost: MEMORY_KIB, timeCost: TIME_COST, parallelism: PARALLELISM });
}

/** Compares `raw` with a stored hash in constant time. An unknown or broken hash gives `false`. */
export function checkPassword(raw: string, encoded: string): boolean {
  if (encoded.startsWith("$argon2")) {
    try {
      return verifySync(encoded, raw);
    } catch {
      return false;
    }
  }
  if (encoded.startsWith("pbkdf2_sha256$")) return checkPbkdf2(raw, encoded);
  return false;
}

/** True when the hash is not argon2id with the current parameters. */
export function needsUpdate(encoded: string): boolean {
  const match = PARAMS.exec(encoded);
  return match === null || match[1] !== String(MEMORY_KIB) || match[2] !== String(TIME_COST) || match[3] !== String(PARALLELISM);
}

/** Django format: `pbkdf2_sha256$<iterations>$<salt>$<base64 digest>`. */
function checkPbkdf2(raw: string, encoded: string): boolean {
  const parts = encoded.split("$");
  if (parts.length !== 4) return false;
  const [, iterations, salt, digest] = parts as [string, string, string, string];
  if (!/^\d+$/.test(iterations) || !BASE64.test(digest)) return false;
  const count = Number(iterations);
  if (!Number.isSafeInteger(count) || count < 1) return false;
  const expected = Buffer.from(digest, "base64");
  const actual = pbkdf2Sync(raw, salt, count, 32, "sha256");
  return actual.length === expected.length && timingSafeEqual(actual, expected);
}
