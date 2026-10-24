//! Read generated schema literals without executing the models or loading extension libraries.
use std::path::{Path, PathBuf};

use orm_core::{dsl, ir::SchemaIr, loader};

pub(super) fn check(schema: &Path, out: Option<&str>, installed: &dsl::Installed) -> Result<(), String> {
    let paths = match out {
        Some(out) => vec![PathBuf::from(out)],
        None => ["models.py", "models.ts"].iter().map(|name| schema.with_file_name(name)).collect(),
    };
    for path in paths {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if out.is_some() { return Err(format!("no generated models at {}; run `orm generate`", path.display())); }
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if !generated(&source) { continue; }
        let locked = embedded(&path, &source).map_err(|e| format!("{}: {e}; run `orm generate`", path.display()))?;
        let mut errors = vec![];
        for (id, stamp) in &locked.behavior.packages {
            let locked_identity = format!("locked version {} hash {}", stamp.version, stamp.manifest_hash);
            let found: Vec<_> = installed.extensions.iter().filter(|(name, _)| name == id).collect();
            let installed_identity = match found.as_slice() {
                [] => "not installed".to_owned(),
                [(_, manifest)] => match loader::read_manifest(id, manifest) {
                    Ok(package) => {
                        if package.manifest.version == stamp.version && package.manifest_hash == stamp.manifest_hash { continue; }
                        format!("installed version {} hash {}", package.manifest.version, package.manifest_hash)
                    }
                    Err(e) => format!("installed manifest unreadable: {e}"),
                },
                _ => format!("installed more than once: {}", found.iter().map(|(_, p)| p.display().to_string()).collect::<Vec<_>>().join(", ")),
            };
            errors.push(format!("extension {id}: {locked_identity}; {installed_identity}; run `orm generate`"));
        }
        if !errors.is_empty() { return Err(errors.join("\n")); }
    }
    Ok(())
}

// Only files carrying a generator header participate; handwritten default names are common.
fn generated(source: &str) -> bool {
    let header = source.lines().next().unwrap_or("");
    header == "# Generated schema module. Do not edit."
        || (header.starts_with("# Generated from ") && header.ends_with(" by `python -m orm generate`. Do not edit."))
        || (header.starts_with("// Generated from ") && header.ends_with(" by `orm generate typescript`. Do not edit."))
}

fn embedded(path: &Path, source: &str) -> Result<SchemaIr, String> {
    // A multi-file Python project exposes facades; the literal lives in their shared module.
    if source.starts_with("# Generated schema module. Do not edit.\n") {
        let literal = source.split_once("_path = (_Path(__file__).parent / ").ok_or("generated facade has no schema path")?.1;
        let relative = serde_json::Deserializer::from_str(literal).into_iter::<String>().next().ok_or("generated facade has no schema path")?.map_err(|e| e.to_string())?;
        let shared = path.parent().unwrap_or(Path::new(".")).join(relative);
        let source = std::fs::read_to_string(&shared).map_err(|e| format!("{}: {e}", shared.display()))?;
        return literal_schema(&source);
    }
    literal_schema(source)
}

fn literal_schema(source: &str) -> Result<SchemaIr, String> {
    if let Some((_, rest)) = source.split_once("_SCHEMA = r\"\"\"\n") {
        let json = rest.split_once("\n\"\"\"").ok_or("unterminated generated Python schema")?.0;
        return SchemaIr::from_json(json).map_err(|e| e.to_string());
    }
    if let Some((_, rest)) = source.split_once("const SCHEMA: SchemaIR = ") {
        // Parse one JSON value: strings in it can contain the generator's closing delimiter.
        return serde_json::Deserializer::from_str(rest).into_iter::<SchemaIr>().next().ok_or("missing generated TypeScript schema")?.map_err(|e| e.to_string());
    }
    Err("no generated schema literal".into())
}
