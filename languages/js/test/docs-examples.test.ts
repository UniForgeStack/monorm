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
const api = pathToFileURL(join(ROOT, "languages/js/dist/src/index.js")).href;

interface Block { info: string[]; line: number; source: string }

function parseFences(source: string): Block[] {
  const blocks: Block[] = [];
  let opening: string | undefined;
  let block: Block | undefined;
  let body: string[] = [];
  for (const [index, line] of source.split(/\r?\n/).entries()) {
    if (opening === undefined) {
      const match = /^\s*(`{3,})([^`]*)$/.exec(line);
      if (match) {
        opening = match[1]!;
        block = { info: match[2]!.trim().split(/\s+/).filter(Boolean), line: index + 2, source: "" };
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
  return blocks;
}

function options(source: string): Record<string, string> {
  const comment = /<!--\s*docs-test:\s*(.*?)\s*-->/.exec(source);
  return Object.fromEntries((comment?.[1]?.trim().split(/\s+/) ?? []).map((word) => word.split("=")));
}

function docsIn(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? docsIn(path) : path.endsWith(".md") ? [path] : [];
  }).sort();
}

function runDoc(doc: string) {
  const source = readFileSync(doc, "utf8");
  const config = options(source);
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
      const url = config["db"] === "postgresql" ? process.env["ORM_DOCS_DATABASE_URL"] : "sqlite://:memory:";
      program += `const docsSchema = ${JSON.stringify(schema)};\nconst docsDatabaseUrl = ${JSON.stringify(url)};\n`;
      program += readFileSync(join(FIXTURES, `${setup}.ts`), "utf8") + "\n";
    }
    for (const block of blocks) {
      if (block.info[0] !== "typescript" || !block.info.includes("run")) continue;
      const label = `${doc}:${block.line}: ${block.source.split("\n")[0] || "<empty>"}`;
      program += `docsTestBlock = ${JSON.stringify(label)};\n`;
      const expected = block.info.find((word) => word.startsWith("fail="))?.slice(5);
      if (expected) {
        program += `{
          let caught = false;
          try { ${block.source} } catch (error) {
            if (error?.name !== ${JSON.stringify(expected)}) throw error;
            caught = true;
          }
          if (!caught) throw new Error(${JSON.stringify(`expected ${expected}, block passed`)});
        }\n`;
      } else {
        program += block.source + "\n";
      }
    }
    program = program.replace(/(["'])@ufstack\/monorm\1/g, JSON.stringify(api));
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
      finally { if (docsTestCleanup) await docsTestCleanup(); }
    `;
    writeFileSync(join(directory, "examples.mts"), moduleSource);
    const compiled = ts.transpileModule(moduleSource, {
      fileName: "examples.mts", compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
      reportDiagnostics: true,
    });
    const errors = compiled.diagnostics?.filter((item) => item.category === ts.DiagnosticCategory.Error) ?? [];
    assert.equal(errors.length, 0, `${doc}: ${errors.map((item) => ts.flattenDiagnosticMessageText(item.messageText, "\n")).join("\n")}`);
    const modulePath = join(directory, "examples.mjs");
    writeFileSync(modulePath, compiled.outputText);
    return spawnSync(process.execPath, [modulePath], { cwd: directory, encoding: "utf8", timeout: 60_000 });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("runnable doc blocks run", async (t) => {
  const docs = docsIn(join(ROOT, "docs")).filter((path) => parseFences(readFileSync(path, "utf8"))
    .some((block) => block.info[0] === "typescript" && block.info.includes("run")));
  for (const doc of [...docs, join(FIXTURES, "execution-cases.md"), join(FIXTURES, "postgresql.md")]) {
    const skip = options(readFileSync(doc, "utf8"))["db"] === "postgresql" && !process.env["ORM_DOCS_DATABASE_URL"];
    // case: docs_examples::runnable_doc_blocks_run
    await t.test(relative(ROOT, doc), { skip: skip ? "ORM_DOCS_DATABASE_URL is unset: PostgreSQL doc skipped" : false }, () => {
      const result = runDoc(doc);
      assert.equal(result.status, 0, `${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    });
  }
});

test("the fence parser reads the fixture", () => {
  assert.deepEqual(parseFences(readFileSync(join(FIXTURES, "parser-cases.md"), "utf8")),
    JSON.parse(readFileSync(join(FIXTURES, "parser-cases.json"), "utf8")));
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
      ['throw new TypeError("wrong error");', "wrong error"]]) {
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
