//! The types generated for an abstract model (`behavior.type_models`), checked by mypy --strict, pyright and tsc:
//! a query-set class and a methods class written against the abstract type work on the model that extends it.
use orm_core::codegen::{python, typescript, Options};
use std::{fs, path::{Path, PathBuf}, process::Command};

const SCHEMA: &str = "model User {\n id Int @id\n email String\n}\nmodel Post {\n id Int @id\n}";

const QUERIES_PY: &str = "from __future__ import annotations

from typing import TYPE_CHECKING

from typing_extensions import Self

from orm import QuerySet

if TYPE_CHECKING:
    from models import AbstractUserFields


class UserQueries[M: AbstractUserFields](QuerySet[M]):
    def by_email(self, email: str) -> Self:
        return self.filter(self._model.email == email)
";

const METHODS_PY: &str = "from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from models import AbstractUserFields as _Fields
else:
    _Fields = object


class UserMethods(_Fields):
    def domain(self) -> str:
        return self.email.split(\"@\")[1]
";

const ACCEPTED_PY: &str = "from models import AbstractUserFields, Post, User


async def f() -> str:
    user = await User.objects.by_email(\"a@b.c\").get()
    assert user is not None
    fields: AbstractUserFields = user
    return user.domain() + fields.email + str(Post.objects.count)
";

const QUERIES_TS: &str = "import { QuerySet } from \"orm\";
import type { AbstractUserSpec } from \"./models.js\";

export class UserQueries<S extends AbstractUserSpec> extends QuerySet<S> {
  byEmail(email: string): this {
    return this.filter({ email } as never) as never;
  }
}
";

const METHODS_TS: &str = "import type { AbstractUserData } from \"./models.js\";

export class UserMethods {
  domain(this: AbstractUserData): string {
    return this.email.split(\"@\")[1] ?? \"\";
  }
}
";

const ACCEPTED_TS: &str = "import { Post, User } from \"./models.js\";
import type { AbstractUserData, AbstractUserSpec } from \"./models.js\";

export async function f(): Promise<string> {
  const user = await User.objects.byEmail(\"a@b.c\").first();
  const data: AbstractUserData | null = user;
  const spec: AbstractUserSpec | null = null;
  return (user?.domain() ?? \"\") + (data?.email ?? \"\") + String(spec) + String(Post.objects);
}
";

fn host() -> PathBuf {
    let start = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();
    start.ancestors().find(|d| d.join("Cargo.lock").is_file()).expect("repo root").to_path_buf()
}

fn project(language: &str, queries: &str) -> (PathBuf, orm_core::ir::SchemaIr, orm_core::schema::Schema, Options) {
    let dir = host().join("target/abstract-typing").join(format!("{language}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut ir = orm_core::dsl::compile(SCHEMA, None).unwrap();
    ir.behavior.type_models = serde_json::from_value(serde_json::json!([{
        "name": "AbstractUser",
        "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}, {"name": "email", "column": "email", "type": "string"}],
        "extended_by": ["User"]
    }]))
    .unwrap();
    ir.behavior.schema_contract = orm_contracts::extension::SCHEMA_CONTRACT;
    ir.models[0].classes_from = Some("AbstractUser".into());
    let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
    let options = Options {
        query_sets: [("User".into(), queries.into())].into(),
        model_methods: [("User".into(), queries.replace("queries", "methods").replace("UserQueries", "UserMethods"))].into(),
        ..Options::default()
    };
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
fn python_query_set_and_methods_class_accept_the_abstract_type() {
    let (dir, ir, schema, options) = project("python", "queries:UserQueries");
    let generated = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
    let python_host = host().join("languages/python");
    write(&dir, "models.py", &generated.module);
    write(&dir, "models.pyi", &generated.stub);
    write(&dir, "queries.py", QUERIES_PY);
    write(&dir, "methods.py", METHODS_PY);
    write(&dir, "mypy.ini", &format!("[mypy]\nstrict = True\nmypy_path = {}\nexplicit_package_bases = True\ncache_dir = {}\n", python_host.display(), dir.join("mypy-cache").display()));
    write(&dir, "pyrightconfig.json", &format!("{{\"typeCheckingMode\": \"strict\", \"extraPaths\": [{:?}], \"reportIncompatibleMethodOverride\": false, \"reportIncompatibleVariableOverride\": false}}", python_host));
    write(&dir, "accepted.py", ACCEPTED_PY);
    let mypy = |file: &str| { let mut c = Command::new("mypy"); c.args(["--config-file", "mypy.ini", file]); c };
    let pyright = |file: &str| { let mut c = Command::new("pyright"); c.args(["--project", "pyrightconfig.json", file]); c };
    run(&dir, &mut mypy("accepted.py"), None, "mypy");
    run(&dir, &mut pyright("accepted.py"), None, "pyright");
    for (expression, member) in [("User.objects.by_email(1)", "email"), ("Post.objects.by_email(\"a\")", "by_email")] {
        write(&dir, "rejected.py", &format!("from models import Post, User\n\nx = {expression}\n"));
        run(&dir, &mut mypy("rejected.py"), Some(member), "mypy");
        run(&dir, &mut pyright("rejected.py"), Some(member), "pyright");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn typescript_query_set_and_methods_class_accept_the_abstract_type() {
    let (dir, ir, schema, options) = project("typescript", "./queries.js#UserQueries");
    let generated = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
    let js = host().join("languages/js");
    write(&dir, "models.ts", &generated);
    write(&dir, "queries.ts", QUERIES_TS);
    write(&dir, "methods.ts", METHODS_TS);
    write(&dir, "tsconfig.json", &format!("{{\"compilerOptions\": {{\"target\": \"ES2022\", \"lib\": [\"ES2022\"], \"module\": \"ESNext\", \"moduleResolution\": \"Bundler\",
        \"types\": [\"node\"], \"typeRoots\": [{:?}], \"strict\": true, \"noUncheckedIndexedAccess\": true, \"exactOptionalPropertyTypes\": true,
        \"noImplicitOverride\": true, \"verbatimModuleSyntax\": true, \"noEmit\": true, \"skipLibCheck\": true,
        \"paths\": {{\"orm\": [{:?}], \"orm/ext\": [{:?}]}}}}, \"include\": [\"accepted.ts\", \"models.ts\", \"queries.ts\", \"methods.ts\"]}}",
        js.join("node_modules/@types"), js.join("src/index.ts"), js.join("src/ext.ts")));
    write(&dir, "accepted.ts", ACCEPTED_TS);
    let local = js.join("node_modules/.bin/tsc");
    let tsc = || Command::new(if local.exists() { local.clone() } else { PathBuf::from("tsc") });
    run(&dir, tsc().args(["-p", "tsconfig.json"]), None, "tsc");
    for (expression, member) in [("User.objects.byEmail(1)", "TS2345"), ("Post.objects.byEmail(\"a\")", "byEmail")] {
        write(&dir, "accepted.ts", &format!("import {{ Post, User }} from \"./models.js\";\n\nexport const x = {expression};\n"));
        run(&dir, tsc().args(["-p", "tsconfig.json"]), Some(member), "tsc");
    }
    let _ = fs::remove_dir_all(&dir);
}
