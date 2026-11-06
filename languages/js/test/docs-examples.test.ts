import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { test } from "node:test";
import ts from "typescript";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
const FIXTURES = join(ROOT, "fixtures/docs");
const PACKAGE = join(ROOT, "languages/js");
const EXPORTS = JSON.parse(readFileSync(join(PACKAGE, "package.json"), "utf8")).exports as Record<string, { default: string }>;
const OPTIONS: Record<string, string[] | undefined> = { setup: undefined, db: ["sqlite", "postgresql"] };
const POSTGRESQL_SKIPPED = "ORM_DOCS_DATABASE_URL is unset: PostgreSQL doc skipped";
const DEFAULT_TEST_URL = "postgres://postgres:postgres@localhost/orm_test";
const FENCE = /^\s*(`{3,}|~{3,})([\s\S]*)$/;

interface Block { info: string[]; line: number; source: string }
interface Outcome { status: number | null; stdout: string; stderr: string; error?: Error }

function parseFences(source: string): Block[] {
  const blocks: Block[] = [];
  let opening: string | undefined;
  let opened = 0;
  let block: Block | undefined;
  let body: string[] = [];
  for (const [index, line] of source.split(/\r?\n/).entries()) {
    if (opening === undefined) {
      const match = FENCE.exec(line);
      if (match && !(match[1]![0] === "`" && match[2]!.includes("`"))) {
        const words = match[2]!.trim().split(/\s+/).filter(Boolean);
        if (match[1]![0] === "~" && words.includes("run")) {
          throw new Error(`line ${index + 1}: a ~~~ fence cannot run; use \`\`\``);
        }
        opening = match[1]!;
        opened = index + 1;
        block = { info: words, line: index + 2, source: "" };
        body = [];
      }
    } else if (line.trim() === opening) {
      block!.source = body.join("\n") + "\n";
      blocks.push(block!);
      opening = undefined;
    } else {
      body.push(line);
    }
  }
  if (opening !== undefined) throw new Error(`line ${opened}: unclosed fence`);
  return blocks;
}

function options(source: string): Record<string, string> {
  const comment = /<!--\s*docs-test:\s*(.*?)\s*-->/.exec(source);
  const config: Record<string, string> = {};
  for (const word of comment?.[1]?.trim().split(/\s+/).filter(Boolean) ?? []) {
    const separator = word.indexOf("=");
    const key = separator < 0 ? word : word.slice(0, separator);
    const value = separator < 0 ? "" : word.slice(separator + 1);
    if (!(key in OPTIONS) || !value) throw new Error(`docs-test: unknown option '${word}'`);
    const allowed = OPTIONS[key];
    if (allowed && !allowed.includes(value)) {
      throw new Error(`docs-test: ${key} must be one of ${JSON.stringify(allowed)}, not '${value}'`);
    }
    config[key] = value;
  }
  return config;
}

const isRun = (block: Block, language: string) => block.info[0] === language && block.info.includes("run");
const firstLine = (source: string) => source.split("\n")[0] || "<empty>";

function databaseKey(url: string): string {
  const parts = new URL(url);
  return `${parts.hostname}:${parts.port || "5432"}${parts.pathname}`;
}

function skipReason(doc: string): string | undefined {
  const postgresql = options(readFileSync(doc, "utf8"))["db"] === "postgresql";
  return postgresql && !process.env["ORM_DOCS_DATABASE_URL"] ? POSTGRESQL_SKIPPED : undefined;
}

function docsIn(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? docsIn(path) : path.endsWith(".md") ? [path] : [];
  }).sort();
}

function syntaxErrors(source: string): string[] {
  const compiled = ts.transpileModule(source, {
    fileName: "examples.mts", compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
    reportDiagnostics: true,
  });
  return (compiled.diagnostics ?? []).filter((item) => item.category === ts.DiagnosticCategory.Error)
    .map((item) => ts.flattenDiagnosticMessageText(item.messageText, "\n"));
}

const failed = (message: string): Outcome => ({ status: 1, stdout: "", stderr: message });

function runDoc(doc: string, env: NodeJS.ProcessEnv = process.env): Outcome {
  const source = readFileSync(doc, "utf8");
  const config = options(source);
  let url = "sqlite://:memory:";
  if (config["db"] === "postgresql") {
    url = env["ORM_DOCS_DATABASE_URL"]!;
    if (databaseKey(url) === databaseKey(env["ORM_TEST_DATABASE_URL"] ?? DEFAULT_TEST_URL)) {
      return failed("ORM_DOCS_DATABASE_URL must not be the test database: the doc drops its tables");
    }
  }
  const directory = mkdtempSync(join(tmpdir(), "monorm-docs-"));
  try {
    const blocks = parseFences(source);
    for (const block of blocks) {
      for (const word of block.info) {
        if (word.startsWith("file=")) {
          const target = resolve(directory, word.slice(5));
          assert.ok(target.startsWith(directory + sep), "file= must stay inside the doc work directory");
          mkdirSync(dirname(target), { recursive: true });
          writeFileSync(target, block.source);
        }
      }
    }
    let program = "";
    const setup = config["setup"];
    if (setup) {
      let schema = readFileSync(join(FIXTURES, `${setup}.prisma`), "utf8");
      if (config["db"] === "postgresql") schema = schema.replace('provider = "sqlite"', 'provider = "postgresql"');
      program += `const docsSchema = ${JSON.stringify(schema)};\nconst docsDatabaseUrl = ${JSON.stringify(url)};\n`;
      program += readFileSync(join(FIXTURES, `${setup}.ts`), "utf8") + "\n";
    }
    for (const block of blocks) {
      if (!isRun(block, "typescript")) continue;
      const label = `${doc}:${block.line}: ${firstLine(block.source)}`;
      const expected = block.info.find((word) => word.startsWith("fail="))?.slice(5);
      const piece = expected ? `{
          let caught = false;
          try { ${block.source} } catch (error) {
            if (error?.name !== ${JSON.stringify(expected)}) throw error;
            caught = true;
          }
          if (!caught) throw new Error(${JSON.stringify(`expected ${expected}, block passed`)});
        }` : block.source;
      const errors = syntaxErrors(piece);
      if (errors.length) return failed(`${label}\n${errors.join("\n")}`);
      program += `docsTestBlock = ${JSON.stringify(label)};\n${piece}\n`;
    }
    let unknown = "";
    program = program.replace(/(["'])@ufstack\/monorm(\/[^"'\s\\]*)?\1/g, (_match, _quote, subpath: string | undefined) => {
      const target = EXPORTS[subpath ? `.${subpath}` : "."]?.default;
      if (!target) unknown = `@ufstack/monorm${subpath ?? ""}`;
      return JSON.stringify(pathToFileURL(join(PACKAGE, target ?? "")).href);
    });
    if (unknown) return failed(`${doc}: ${unknown} is not an export of the package`);
    // Keep imports at module scope while the shared namespace gets one try/finally.
    const parsed = ts.createSourceFile("examples.mts", program, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
    const imports: string[] = [];
    const body: string[] = [];
    for (const statement of parsed.statements) {
      (ts.isImportDeclaration(statement) ? imports : body).push(statement.getText(parsed));
    }
    const moduleSource = `${imports.join("\n")}
      let docsTestCleanup;
      let docsTestBlock = ${JSON.stringify(`${doc}: setup`)};
      try { ${body.join("\n")} }
      catch (error) { throw new Error(docsTestBlock + "\\n" + error, { cause: error }); }
      finally {
        if (docsTestCleanup) {
          try { await docsTestCleanup(); }
          catch (error) { console.error(${JSON.stringify(`${doc}: cleanup`)} + "\\n" + error); process.exitCode = 1; }
        }
      }
    `;
    const errors = syntaxErrors(moduleSource);
    if (errors.length) return failed(`${doc}\n${errors.join("\n")}`);
    const modulePath = join(directory, "examples.mjs");
    writeFileSync(modulePath, ts.transpileModule(moduleSource, {
      fileName: "examples.mts", compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
    }).outputText);
    return spawnSync(process.execPath, [modulePath], { cwd: directory, encoding: "utf8", timeout: 60_000, env });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("runnable doc blocks run", async (t) => {
  const docs = docsIn(join(ROOT, "docs")).filter((path) => parseFences(readFileSync(path, "utf8"))
    .some((block) => isRun(block, "typescript")));
  for (const doc of [...docs, join(FIXTURES, "execution-cases.md"), join(FIXTURES, "postgresql.md")]) {
    const skip = skipReason(doc);
    // case: docs_examples::runnable_doc_blocks_run
    await t.test(relative(ROOT, doc), { skip: skip ?? false }, () => {
      const result = runDoc(doc);
      assert.equal(result.status, 0, `${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    });
  }
});

test("the fence parser reads the fixture", () => {
  assert.deepEqual(parseFences(readFileSync(join(FIXTURES, "parser-cases.md"), "utf8")),
    JSON.parse(readFileSync(join(FIXTURES, "parser-cases.json"), "utf8")));
});

test("the fence parser reads the source cases", () => {
  const cases = JSON.parse(readFileSync(join(FIXTURES, "source-cases.json"), "utf8")) as
    { name: string; source: string; blocks?: Block[]; error?: string }[];
  for (const item of cases) {
    if (item.error) assert.throws(() => parseFences(item.source), (error: Error) => error.message.includes(item.error!), item.name);
    else assert.deepEqual(parseFences(item.source), item.blocks, item.name);
  }
});

test("the option parser reads the fixture", () => {
  const cases = JSON.parse(readFileSync(join(FIXTURES, "option-cases.json"), "utf8")) as
    { source: string; options?: Record<string, string>; error?: string }[];
  for (const item of cases) {
    if (item.error) assert.throws(() => options(item.source), (error: Error) => error.message.includes(item.error!), item.source);
    else assert.deepEqual(options(item.source), item.options, item.source);
  }
});

test("a failing block names the doc and the block", () => {
  const doc = join(FIXTURES, "broken.md");
  const result = runDoc(doc);
  assert.notEqual(result.status, 0);
  assert.ok(result.stderr.includes(doc), result.stderr);
  assert.ok(result.stderr.includes("await User.objects.noSuchMethod()"), result.stderr);
  const directory = mkdtempSync(join(tmpdir(), "monorm-docs-errors-"));
  try {
    for (const [body, message] of [[";", "expected RangeError, block passed"],
      ['throw new TypeError("wrong error");', "wrong error"],
      ["const = ;", ":2: const = ;"]]) {
      const expectedDoc = join(directory, "expected-error.md");
      writeFileSync(expectedDoc, `\`\`\`typescript run fail=RangeError\n${body}\n\`\`\`\n`);
      const result = runDoc(expectedDoc);
      assert.notEqual(result.status, 0);
      assert.ok(result.stderr.includes(expectedDoc), result.stderr);
      assert.ok(result.stderr.includes(message!), result.stderr);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("a doc database cannot be the test database", () => {
  const url = "postgres://postgres:postgres@localhost/shared";
  const result = runDoc(join(FIXTURES, "postgresql.md"), { ...process.env, ORM_DOCS_DATABASE_URL: url, ORM_TEST_DATABASE_URL: url });
  assert.notEqual(result.status, 0);
  assert.ok(result.stderr.includes("must not be the test database"), result.stderr);
});
