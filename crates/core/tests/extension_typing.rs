//! The types generated for extension language code, checked by mypy --strict, pyright and tsc:
//! a member is accepted on a model that has the extension and rejected on a model without it.
//! The first extension is the fixture of the extension build; the second composes with it.
use orm_contracts::extension::{Declaration, LanguageMembers, SourceLocation};
use orm_core::codegen::{python, typescript, ExtensionCode, Options};
use std::{fs, path::{Path, PathBuf}, process::Command};

const SCHEMA: &str = "model User {\n id Int @id\n}\nmodel Post {\n id Int @id\n}\nmodel Note {\n id Int @id\n}";

const SECOND_PY: &str = "from __future__ import annotations

from typing_extensions import Self

from orm import QuerySet
from orm.ext import ModelLike, ModelT


class SecondQuerySet(QuerySet[ModelT]):
    def without_defaults(self, *names: str) -> Self:
        return super().without_defaults(*names)

    def second(self) -> Self:
        return self.without_defaults()


class SecondInstance:
    def shout(self: ModelLike) -> str:
        return str(self.pk).upper()
";

const SECOND_TS: &str = "import type { ModelSpec, QuerySet } from \"orm\";
import type { QuerySetCtor } from \"orm/ext\";

type AnyQuerySet = QuerySet<ModelSpec>;

export function secondQuerySet<B extends QuerySetCtor>(Base: B) {
  return class Second extends Base {
    override withoutDefaults<T extends AnyQuerySet>(this: T): T {
      return super.withoutDefaults() as unknown as T;
    }

    second<T extends AnyQuerySet>(this: T): T {
      return this.withoutDefaults();
    }
  };
}

export class SecondInstance {
  shout(this: { readonly pk: unknown }): string {
    return String(this.pk).toUpperCase();
  }
}
";

const ACCEPTED_PY: &str = "from models import Note, Post, User
from orm import QuerySet


def queries() -> None:
    a: QuerySet[Post] = Post.objects.everything().second().everything()
    b: QuerySet[Post] = Post.objects.second().without_defaults().everything()
    c: QuerySet[Note] = Note.objects.everything().without_defaults().everything()
    d: QuerySet[User] = User.objects.without_defaults()
    print(a, b, c, d)


def instances(post: Post, note: Note) -> str:
    return post.label() + post.shout() + note.label() + post.me().label()
";

const ACCEPTED_TS: &str = "import type { QuerySet } from \"orm\";
import { Note, Post, User } from \"./models.js\";
import type { NoteSpec, PostSpec, UserSpec } from \"./models.js\";

export const a: QuerySet<PostSpec> = Post.objects.everything().second().everything();
export const b: QuerySet<PostSpec> = Post.objects.second().withoutDefaults().everything();
export const c: QuerySet<NoteSpec> = Note.objects.everything().withoutDefaults().everything();
export const d: QuerySet<UserSpec> = User.objects.withoutDefaults();

export function instances(post: Post, note: Note): string {
  return post.label() + post.shout() + note.label();
}
";

// (expression, the member the checkers must name)
const REJECTED: &[(&str, &str)] = &[
    ("User.objects.everything()", "everything"),
    ("Note.objects.second()", "second"),
    ("User.objects.second()", "second"),
];

fn code(id: &str, attribute: &str, language: &str, query_set: &str, instance: &str, files: Vec<(String, String)>) -> ExtensionCode {
    ExtensionCode {
        members: LanguageMembers {
            extension: id.into(), version: "1.0.0".into(), language: language.into(),
            query_set: query_set.into(), instance: instance.into(), hash: "0".into(), ..Default::default()
        },
        attributes: vec![attribute.into()],
        files,
    }
}

// A feature-check artifact copies only some crates and builds under the repo's `target/`, so walk up to the fixtures.
fn host() -> PathBuf {
    let start = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();
    start.ancestors().find(|d| d.join("crates/extension-build/tests/fixtures").is_dir()).expect("repo root").to_path_buf()
}

fn fixture(dir: &str) -> Vec<(String, String)> {
    let root = host().join("crates/extension-build/tests/fixtures/language").join(dir);
    let mut files: Vec<(String, String)> = fs::read_dir(&root).unwrap().map(|e| e.unwrap().path())
        .filter(|p| p.is_file()).map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&p).unwrap())).collect();
    files.sort();
    files
}

fn declare(ir: &mut orm_core::ir::SchemaIr, attribute: &str, models: &[&str]) {
    for model in models {
        ir.behavior.declarations.push(Declaration {
            attribute: attribute.into(), model: (*model).into(), field: None, field_type: None,
            arguments: Default::default(), positional: vec![],
            location: SourceLocation { file: "schema.prisma".into(), line: 1, column: 1 }, lowered: false,
        });
    }
}

fn project(language: &str) -> (PathBuf, orm_core::ir::SchemaIr, orm_core::schema::Schema, Options) {
    let dir = host().join("target/extension-typing").join(format!("{language}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut ir = orm_core::dsl::compile(SCHEMA, None).unwrap();
    let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
    declare(&mut ir, "example.tagged", &["Post", "Note"]);
    declare(&mut ir, "example.second", &["Post"]);
    let (first, second) = if language == "python" {
        (code("example.language", "example.tagged", language, "TaggedQuerySet", "TaggedInstance", fixture("python/example_language")),
         code("example.second", "example.second", language, "SecondQuerySet", "SecondInstance", vec![("__init__.py".into(), SECOND_PY.into())]))
    } else {
        (code("example.language", "example.tagged", language, "taggedQuerySet", "TaggedInstance", fixture("typescript/src")),
         code("example.second", "example.second", language, "secondQuerySet", "SecondInstance", vec![("index.ts".into(), SECOND_TS.into())]))
    };
    let options = Options { extensions: vec![first, second], ..Options::default() };
    (dir, ir, schema, options)
}

fn write(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn run(dir: &Path, program: &mut Command, expect_error: Option<&str>, label: &str) {
    let out = program.current_dir(dir).output().unwrap_or_else(|e| panic!("{label}: cannot run: {e}; activate the virtual environment and install the npm packages"));
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    match expect_error {
        None => assert!(out.status.success(), "{label} must accept the project:\n{text}"),
        Some(member) => assert!(!out.status.success() && text.contains(member), "{label} must reject {member}:\n{text}"),
    }
}

#[test]
fn python_members_type_check_on_the_models_that_have_them() {
    let (dir, ir, schema, options) = project("python");
    let generated = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
    let python_host = host().join("languages/python");
    write(&dir, "models.py", &generated.module);
    write(&dir, "models.pyi", &generated.stub);
    write(&dir, "orm_ext/__init__.py", "");
    for ext in &options.extensions { for (name, content) in ext.generated_files("python") { write(&dir.join("orm_ext"), &name, &content); } }
    write(&dir, "mypy.ini", &format!("[mypy]\nstrict = True\nmypy_path = {}\nexplicit_package_bases = True\ncache_dir = {}\n", python_host.display(), dir.join("mypy-cache").display()));
    write(&dir, "pyrightconfig.json", &format!("{{\"typeCheckingMode\": \"strict\", \"extraPaths\": [{:?}]}}", python_host));
    write(&dir, "accepted.py", ACCEPTED_PY);
    let mypy = |file: &str| { let mut c = Command::new("mypy"); c.args(["--config-file", "mypy.ini", file]); c };
    let pyright = |file: &str| { let mut c = Command::new("pyright"); c.args(["--project", "pyrightconfig.json", file]); c };
    run(&dir, &mut mypy("accepted.py"), None, "mypy");
    run(&dir, &mut pyright("accepted.py"), None, "pyright");
    for (expression, member) in REJECTED {
        write(&dir, "rejected.py", &format!("from models import Note, User\n\nx = {expression}\n"));
        run(&dir, &mut mypy("rejected.py"), Some(member), "mypy");
        run(&dir, &mut pyright("rejected.py"), Some(member), "pyright");
    }
    write(&dir, "rejected.py", "from models import Note, User\n\n\ndef f(user: User, note: Note) -> str:\n    return user.label() + note.shout()\n");
    run(&dir, &mut mypy("rejected.py"), Some("label"), "mypy");
    run(&dir, &mut pyright("rejected.py"), Some("shout"), "pyright");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn typescript_members_type_check_on_the_models_that_have_them() {
    let (dir, ir, schema, options) = project("typescript");
    let generated = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
    let js = host().join("languages/js");
    write(&dir, "models.ts", &generated);
    for ext in &options.extensions { for (name, content) in ext.generated_files("typescript") { write(&dir.join("orm-ext"), &name, &content); } }
    write(&dir, "tsconfig.json", &format!("{{\"compilerOptions\": {{\"target\": \"ES2022\", \"lib\": [\"ES2022\"], \"module\": \"ESNext\", \"moduleResolution\": \"Bundler\",
        \"types\": [\"node\"], \"typeRoots\": [{:?}], \"strict\": true, \"noUncheckedIndexedAccess\": true, \"exactOptionalPropertyTypes\": true,
        \"noImplicitOverride\": true, \"verbatimModuleSyntax\": true, \"noEmit\": true, \"skipLibCheck\": true,
        \"paths\": {{\"orm\": [{:?}], \"orm/ext\": [{:?}]}}}}, \"include\": [\"accepted.ts\", \"models.ts\", \"orm-ext/**/*.ts\"]}}",
        js.join("node_modules/@types"), js.join("src/index.ts"), js.join("src/ext.ts")));
    write(&dir, "accepted.ts", ACCEPTED_TS);
    let local = js.join("node_modules/.bin/tsc");
    let tsc = || Command::new(if local.exists() { local.clone() } else { PathBuf::from("tsc") });
    run(&dir, tsc().args(["-p", "tsconfig.json"]), None, "tsc");
    for (expression, member) in REJECTED {
        write(&dir, "accepted.ts", &format!("import {{ Note, User }} from \"./models.js\";\n\nexport const x = {expression};\n"));
        run(&dir, tsc().args(["-p", "tsconfig.json"]), Some(member), "tsc");
    }
    write(&dir, "accepted.ts", "import type { Note, User } from \"./models.js\";\n\nexport function f(user: User, note: Note): string {\n  return user.label() + note.shout();\n}\n");
    run(&dir, tsc().args(["-p", "tsconfig.json"]), Some("label"), "tsc");
    let _ = fs::remove_dir_all(&dir);
}
