#!/usr/bin/env node
import { main } from "@ufstack/monorm/cli";

main(process.argv.slice(2)).then(
  (code) => process.exit(code),
  (e) => {
    process.stderr.write(`error: ${e instanceof Error ? e.message : String(e)}\n`);
    process.exit(1);
  },
);
