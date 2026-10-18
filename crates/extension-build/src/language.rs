//! Language code of an extension: collect and hash the sources, scan the member
//! names, and type-check the code against the host packages.
use orm_contracts::extension::{LanguageMembers, LanguageSection, Manifest};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One extension language: its files and the member record the artifact keeps.
#[derive(Debug, Clone)]
pub struct Embedded {
    pub members: LanguageMembers,
    pub dir: PathBuf,
    /// Path relative to the source directory, with the content.
    pub files: Vec<(String, String)>,
}

fn extensions(language: &str) -> &'static [&'static str] {
    if language == "python" { &["py", "pyi"] } else { &["ts"] }
}

fn walk(dir: &Path, root: &Path, wanted: &[&str], out: &mut Vec<(String, String)>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') || name == "node_modules" || name == "__pycache__" { continue; }
        if path.is_dir() {
            walk(&path, root, wanted, out)?;
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| wanted.contains(&e)) {
            let relative = path.strip_prefix(root).unwrap().components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            out.push((relative, fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?));
        }
    }
    Ok(())
}

/// The content hash covers each file name and content in path order.
pub fn hash(files: &[(String, String)]) -> String {
    let mut sha = Sha256::new();
    for (name, content) in files {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update((content.len() as u64).to_le_bytes());
        sha.update(content.as_bytes());
    }
    sha.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Replace comments and string contents with spaces, keeping newlines and braces outside them.
fn blank(source: &str, python: bool) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let line_comment = if python { c == '#' } else { c == '/' && chars.get(i + 1) == Some(&'/') };
        if line_comment {
            while i < chars.len() && chars[i] != '\n' { out.push(' '); i += 1; }
        } else if !python && c == '/' && chars.get(i + 1) == Some(&'*') {
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) { out.push(if chars[i] == '\n' { '\n' } else { ' ' }); i += 1; }
            out.push_str("  ");
            i += 2;
        } else if c == '"' || c == '\'' || (!python && c == '`') {
            let triple = python && chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
            let quote = if triple { 3 } else { 1 };
            out.extend(std::iter::repeat_n(' ', quote));
            i += quote;
            while i < chars.len() {
                if chars[i] == '\\' { out.push_str("  "); i += 2; continue; }
                let closes = chars[i] == c && (!triple || (chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c)));
                if closes { out.extend(std::iter::repeat_n(' ', quote)); i += quote; break; }
                if chars[i] == '\n' && !triple && python { break; }
                out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn ident(text: &str) -> Option<&str> {
    let end = text.find(|c: char| !(c == '_' || c == '$' || c.is_alphanumeric())).unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

/// Public member names of `class <name>` in a Python file set.
fn python_members(files: &[(String, String)], class: &str) -> Option<Vec<String>> {
    for (_, content) in files {
        let text = blank(content, true);
        let lines: Vec<&str> = text.lines().collect();
        let Some(start) = lines.iter().position(|l| l.strip_prefix("class ").and_then(ident) == Some(class)) else { continue };
        let mut names = vec![];
        let mut indent = None;
        for line in &lines[start + 1..] {
            if line.trim().is_empty() { continue; }
            let width = line.len() - line.trim_start().len();
            if width == 0 { break; }
            let indent = *indent.get_or_insert(width);
            if width != indent { continue; }
            let rest = line.trim_start();
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            let name = if let Some(r) = rest.strip_prefix("def ") { ident(r) } else if rest.starts_with("@") { None } else { ident(rest).filter(|n| rest[n.len()..].trim_start().starts_with([':', '='])) };
            if let Some(name) = name { names.push(name.to_owned()); }
        }
        return Some(names);
    }
    None
}

/// Public member names of the class that the TypeScript export `<name>` declares or returns.
fn typescript_members(files: &[(String, String)], export: &str) -> Option<Vec<String>> {
    for (_, content) in files {
        let text = blank(content, false);
        let Some(position) = ["export function ", "export class ", "export const ", "export abstract class "].iter().find_map(|prefix| {
            let mut from = 0;
            while let Some(at) = text[from..].find(prefix) {
                let after = from + at + prefix.len();
                if ident(&text[after..]) == Some(export) { return Some(after); }
                from = after;
            }
            None
        }) else { continue };
        let class = if text[..position].ends_with("class ") { position } else { position + text[position..].find("class ")? };
        let open = class + text[class..].find('{')?;
        let mut depth = 0usize;
        let mut names = vec![];
        let mut line = String::new();
        for c in text[open..].chars() {
            if c == '\n' && depth == 1 {
                let mut rest = line.trim_start();
                for modifier in ["public ", "static ", "async ", "override ", "get ", "set ", "readonly "] {
                    while let Some(r) = rest.strip_prefix(modifier) { rest = r; }
                }
                if let Some(name) = ident(rest) {
                    if rest[name.len()..].trim_start().starts_with(['(', '<', ':', '=']) { names.push(name.to_owned()); }
                }
            }
            if depth == 1 { if c == '\n' { line.clear(); } else { line.push(c); } }
            match c {
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' => { depth -= 1; if depth == 0 { break; } }
                _ => {}
            }
        }
        return Some(names);
    }
    None
}

fn members(language: &str, files: &[(String, String)], export: &str) -> Result<Vec<String>, String> {
    let found = if language == "python" { python_members(files, export) } else { typescript_members(files, export) };
    let mut names: Vec<String> = found.ok_or_else(|| format!("{language}: export {export} not found in the source files"))?
        .into_iter().filter(|n| !n.starts_with('_') && n != "constructor").collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Read the sources of every language section of `manifest`; `package` is the crate directory.
pub fn collect(manifest: &Manifest, package: &Path) -> Result<Vec<Embedded>, String> {
    let mut out = vec![];
    for (language, section) in manifest.language_sections() {
        let dir = package.join(&section.source).canonicalize().map_err(|e| format!("{}: {language} source {}: {e}", manifest.id, section.source))?;
        if !dir.starts_with(package) { return Err(format!("{}: {language} source must be packaged with its crate", manifest.id)); }
        let mut files = vec![];
        walk(&dir, &dir, extensions(language), &mut files)?;
        files.sort();
        if files.is_empty() { return Err(format!("{}: {language} source {} has no source files", manifest.id, section.source)); }
        let query_set_members = members(language, &files, &section.query_set).map_err(|e| format!("{}: {e}", manifest.id))?;
        let instance_members = members(language, &files, &section.instance).map_err(|e| format!("{}: {e}", manifest.id))?;
        for name in &section.overrides {
            if !query_set_members.contains(name) && !instance_members.contains(name) {
                return Err(format!("{}: {language} override {name} is not a member of {} or {}", manifest.id, section.query_set, section.instance));
            }
        }
        let bound_query_set_members = match &section.bound_query_set {
            None => vec![],
            Some(export) => {
                let bound = members(language, &files, export).map_err(|e| format!("{}: {e}", manifest.id))?;
                if let Some(name) = bound.iter().find(|n| !query_set_members.contains(n)) {
                    return Err(format!("{}: {language} bound query-set member {name} of {export} is not a member of {}", manifest.id, section.query_set));
                }
                bound
            }
        };
        let mut overrides = section.overrides.clone();
        overrides.sort();
        out.push(Embedded {
            members: LanguageMembers {
                extension: manifest.id.clone(), version: manifest.version.clone(), language: language.into(),
                query_set: section.query_set.clone(), instance: section.instance.clone(), bound_query_set: section.bound_query_set.clone(),
                query_set_members, instance_members, bound_query_set_members, overrides, hash: hash(&files),
            },
            dir, files,
        });
    }
    Ok(out)
}

fn tool(name: &str, host: &Path) -> PathBuf {
    let local = host.join("languages/js/node_modules/.bin").join(name);
    if name == "tsc" && local.exists() { local } else { PathBuf::from(name) }
}

fn run(mut command: Command, label: &str) -> Result<(), String> {
    let output = command.output().map_err(|e| format!("{label}: cannot run {:?}: {e}; install it or activate the host virtual environment", command.get_program()))?;
    if output.status.success() { return Ok(()); }
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Err(format!("{label}: type check failed\n{}", text.trim()))
}

fn json(value: serde_json::Value) -> String { serde_json::to_string_pretty(&value).expect("json") }

/// Type-check each language of each extension against the host packages; `work` holds the tool configs.
pub fn check(embedded: &[Embedded], host: &Path, work: &Path) -> Result<(), String> {
    for item in embedded {
        let id = &item.members.extension;
        let dir = work.join("language-check").join(format!("{id}-{}", item.members.language));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let label = format!("{id} ({})", item.members.language);
        if item.members.language == "python" {
            let host_python = host.join("languages/python");
            let parent = item.dir.parent().ok_or("python source has no parent")?;
            let path = [host_python.to_string_lossy(), parent.to_string_lossy()].join(":");
            fs::write(dir.join("mypy.ini"), format!("[mypy]\nstrict = True\nmypy_path = {path}\nexplicit_package_bases = True\ncache_dir = {}\n", dir.join("mypy-cache").display())).map_err(|e| e.to_string())?;
            fs::write(dir.join("pyrightconfig.json"), json(serde_json::json!({
                "include": [item.dir], "extraPaths": [host_python, parent], "typeCheckingMode": "strict",
            }))).map_err(|e| e.to_string())?;
            let mut mypy = Command::new("mypy");
            mypy.current_dir(&dir).arg("--config-file").arg(dir.join("mypy.ini")).arg("-p").arg(item.dir.file_name().unwrap());
            run(mypy, &format!("{label} mypy"))?;
            let mut pyright = Command::new("pyright");
            pyright.current_dir(&dir).arg("--project").arg(dir.join("pyrightconfig.json"));
            run(pyright, &format!("{label} pyright"))?;
        } else {
            let js = host.join("languages/js");
            fs::write(dir.join("tsconfig.json"), json(serde_json::json!({
                "compilerOptions": {
                    "target": "ES2022", "lib": ["ES2022"], "module": "ESNext", "moduleResolution": "Bundler",
                    "types": ["node"], "typeRoots": [js.join("node_modules/@types")],
                    "strict": true, "noUncheckedIndexedAccess": true, "exactOptionalPropertyTypes": true,
                    "noImplicitOverride": true, "noFallthroughCasesInSwitch": true, "verbatimModuleSyntax": true,
                    "noEmit": true, "skipLibCheck": true,
                    "paths": { "orm": [js.join("src/index.ts")], "orm/ext": [js.join("src/ext.ts")] },
                },
                "include": [item.dir.join("**/*.ts")],
            }))).map_err(|e| e.to_string())?;
            let mut tsc = Command::new(tool("tsc", host));
            tsc.current_dir(&dir).arg("-p").arg(dir.join("tsconfig.json"));
            run(tsc, &format!("{label} tsc"))?;
        }
    }
    Ok(())
}

/// Rust constants that give the host the embedded files and member records.
pub fn source(embedded: &[Embedded]) -> Result<String, String> {
    let mut files = String::new();
    for item in embedded {
        for (name, content) in &item.files {
            files.push_str(&format!("    ({:?}, {:?}, {:?}, {content:?}),\n", item.members.extension, item.members.language, name));
        }
    }
    Ok(format!("/// (extension, language, file, content) for each embedded language source file.\npub fn language_sources() -> &'static [(&'static str, &'static str, &'static str, &'static str)] {{ &[\n{files}] }}\n"))
}

/// Section rules that need no file access.
pub fn validate_section(manifest: &Manifest, language: &str, section: &LanguageSection) -> Result<(), String> {
    if !manifest.languages.iter().any(|l| l == language) {
        return Err(format!("{}: {language} language section requires {language} in languages", manifest.id));
    }
    for name in [&section.query_set, &section.instance] {
        if name.is_empty() { return Err(format!("{}: {language} section needs a query-set and an instance export", manifest.id)); }
    }
    Ok(())
}
