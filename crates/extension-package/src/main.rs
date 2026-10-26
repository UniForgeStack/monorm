//! Host-platform package builder, independent of the static composition builder.
use std::{
    env,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let usage = "usage: orm-extension-package package <crate-dir> --out <dir> [--tsc <executable>]";
    if args.next().as_deref() != Some("package") {
        return Err(usage.into());
    }
    let dir = PathBuf::from(args.next().ok_or(usage)?).canonicalize()?;
    let mut out = None;
    let mut tsc = "tsc".to_owned();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or(usage)?)),
            "--tsc" => tsc = args.next().ok_or(usage)?,
            _ => return Err(format!("unknown argument {arg}; {usage}").into()),
        }
    }
    let out = out.ok_or(usage)?;
    std::fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let text = std::fs::read_to_string(dir.join("orm-extension.toml"))?;
    let manifest: orm_contracts::extension::Manifest = toml::from_str(&text)?;
    manifest.check_contracts()?;
    if !manifest.is_dynamic() || manifest.library.is_none() {
        return Err("a package needs abi and library".into());
    }
    if !manifest.exports.is_empty() {
        return Err("native exports are not supported by this package builder yet".into());
    }
    let library = manifest.library.as_ref().unwrap();
    if library.is_empty() || library.contains(['/', '\\']) {
        return Err("library must be a file name".into());
    }
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let build = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--lib",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(dir.join("Cargo.toml"))
        .stderr(Stdio::inherit())
        .output()?;
    if !build.status.success() {
        return Err("cargo build failed".into());
    }
    let artifact = String::from_utf8(build.stdout)?
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            v["reason"] == "compiler-artifact"
                && v["manifest_path"]
                    .as_str()
                    .is_some_and(|p| std::path::Path::new(p) == dir.join("Cargo.toml"))
        })
        .flat_map(|v| v["filenames"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(PathBuf::from))
        .find(|p| {
            p.extension()
                .is_some_and(|e| e == env::consts::DLL_EXTENSION)
        })
        .ok_or("crate did not build a cdylib; add crate-type = [\"cdylib\", \"rlib\"]")?;
    let config = serde_json::json!({"dir": dir, "out": out, "library": artifact, "manifest": manifest, "tsc": tsc});
    let mut child = Command::new(env::var_os("PYTHON").unwrap_or_else(|| "python3".into()))
        .args(["-c", include_str!("package.py")])
        .stdin(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&config)?.as_bytes())?;
    if !child.wait()?.success() {
        return Err("package assembly failed".into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
