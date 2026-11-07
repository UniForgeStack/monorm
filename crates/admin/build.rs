//! Lists the files of `ui/dist` (the built SPA) so `src/ui.rs` can embed them. A missing
//! directory gives an empty list: the crate then builds, and the UI answers 404.

use std::fs;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("ui").join("dist");
    println!("cargo:rerun-if-changed=ui/dist");
    println!("cargo:rerun-if-changed=build.rs");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for f in &files {
        let name = f.strip_prefix(&root).expect("under root").to_string_lossy().replace('\\', "/");
        code.push_str(&format!("    ({name:?}, include_bytes!({:?})),\n", f.to_string_lossy()));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("ui_files.rs");
    fs::write(out, code).expect("write ui_files.rs");
}
