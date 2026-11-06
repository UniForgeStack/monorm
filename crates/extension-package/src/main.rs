//! Package builder for pass extensions, independent of the static composition builder.
//!
//! `library` builds the cdylib on the host, `assemble` joins the libraries of the 3 shipped
//! platforms into one wheel and one npm package, and `package` does both for the host only.
use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Error = Box<dyn std::error::Error>;

const USAGE: &str = "usage:
  monorm-extension-package package <crate-dir> --out <dir> [--tsc <executable>] [--features <list>]
  monorm-extension-package library <crate-dir> --out <dir> [--features <list>] [--zigbuild <target>]
  monorm-extension-package assemble <crate-dir> --libraries <dir> --out <dir> [--tsc <executable>]";

/// Platform key, library file prefix and suffix, and whether an outside package ships the platform.
/// The keys name the platform directories of the prebuilt libraries.
const PLATFORMS: [(&str, &str, &str, bool); 6] = [
    ("darwin-arm64", "lib", ".dylib", true),
    ("linux-arm64", "lib", ".so", false),
    ("linux-arm64-musl", "lib", ".so", false),
    ("linux-x64", "lib", ".so", true),
    ("linux-x64-musl", "lib", ".so", false),
    ("win32-x64", "", ".dll", true),
];

/// The platform key of the library that a build makes: the Linux target of `--zigbuild` when there is one, else the host.
/// A musl build runs on a glibc host (or on a Mac), so the host cannot name it.
fn host_platform(zigbuild: Option<&str>) -> Result<&'static str, Error> {
    let (os, arch, musl) = match zigbuild {
        Some(target) if target.contains("-linux-") => (
            "linux",
            target.split('-').next().unwrap_or_default(),
            target.contains("-musl"),
        ),
        _ => (env::consts::OS, env::consts::ARCH, cfg!(target_env = "musl")),
    };
    platform_of(os, arch, musl).ok_or_else(|| {
        format!(
            "no platform key for this host; the keys are {}",
            PLATFORMS.map(|p| p.0).join(", ")
        )
        .into()
    })
}

fn platform_of(os: &str, arch: &str, musl: bool) -> Option<&'static str> {
    Some(match (os, arch, musl) {
        ("macos", "aarch64", false) => "darwin-arm64",
        ("linux", "x86_64", false) => "linux-x64",
        ("linux", "x86_64", true) => "linux-x64-musl",
        ("linux", "aarch64", false) => "linux-arm64",
        ("linux", "aarch64", true) => "linux-arm64-musl",
        ("windows", "x86_64", false) => "win32-x64",
        _ => return None,
    })
}

#[derive(Default)]
struct Args {
    out: Option<PathBuf>,
    libraries: Option<PathBuf>,
    tsc: Option<String>,
    features: Option<String>,
    zigbuild: Option<String>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, Error> {
    let mut args = args;
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| Error::from(USAGE));
        match arg.as_str() {
            "--out" => parsed.out = Some(value()?.into()),
            "--libraries" => parsed.libraries = Some(value()?.into()),
            "--tsc" => parsed.tsc = Some(value()?),
            "--features" => parsed.features = Some(value()?),
            "--zigbuild" => parsed.zigbuild = Some(value()?),
            _ => return Err(format!("unknown argument {arg}; {USAGE}").into()),
        }
    }
    Ok(parsed)
}

fn read_manifest(dir: &Path) -> Result<monorm::extension::Manifest, Error> {
    let text = std::fs::read_to_string(dir.join("monorm-extension.toml"))?;
    let manifest: monorm::extension::Manifest = toml::from_str(&text)?;
    manifest.check_contracts()?;
    if manifest.library.is_none() {
        return Err("a package needs `library`".into());
    }
    for (language, section) in manifest.language_sections() {
        section.check_package_keys(language)?;
    }
    let library = manifest.library.as_ref().unwrap();
    if library.is_empty() || library.contains(['/', '\\']) {
        return Err("library must be a file name".into());
    }
    Ok(manifest)
}

/// The file name the loader opens: a name with an extension is used as it is.
fn library_file(library: &str, prefix: &str, suffix: &str) -> String {
    if Path::new(library).extension().is_some() {
        library.to_owned()
    } else {
        format!("{prefix}{library}{suffix}")
    }
}

fn require_plain(library: &str) -> Result<(), Error> {
    if Path::new(library).extension().is_some() {
        return Err(
            "library must have no file extension; the tool adds the prefix and suffix of each platform"
                .into(),
        );
    }
    Ok(())
}

/// `zigbuild` is a target such as `x86_64-unknown-linux-gnu.2.28`: it builds with `cargo zigbuild`, which sets the glibc floor.
fn build_library(
    dir: &Path,
    features: Option<&str>,
    zigbuild: Option<&str>,
) -> Result<PathBuf, Error> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    // A Linux target makes a `.so`, also when a Mac builds it.
    let extension = match zigbuild {
        Some(target) if target.contains("-linux-") => "so",
        _ => env::consts::DLL_EXTENSION,
    };
    let mut build = Command::new(cargo);
    match zigbuild {
        Some(target) => build.args(["zigbuild", "--target", target]),
        None => build.arg("build"),
    };
    build
        .args([
            "--release",
            "--lib",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(dir.join("Cargo.toml"));
    if let Some(features) = features {
        build.args(["--features", features]);
    }
    let build = build.stderr(Stdio::inherit()).output()?;
    if !build.status.success() {
        return Err("cargo build failed".into());
    }
    String::from_utf8(build.stdout)?
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            v["reason"] == "compiler-artifact"
                && v["manifest_path"]
                    .as_str()
                    .is_some_and(|p| Path::new(p) == dir.join("Cargo.toml"))
        })
        .flat_map(|v| v["filenames"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(PathBuf::from))
        .find(|p| p.extension().is_some_and(|e| e == extension))
        .ok_or_else(|| {
            "crate did not build a cdylib; add crate-type = [\"cdylib\", \"rlib\"]".into()
        })
}

/// `files` are `[source path, file name in the package]`; `tag` is `host` or `platforms`.
fn write_packages(
    dir: &Path,
    out: &Path,
    manifest: &monorm::extension::Manifest,
    files: &[(PathBuf, String)],
    tag: &str,
    tsc: &str,
) -> Result<(), Error> {
    let files: Vec<_> = files
        .iter()
        .map(|(source, name)| serde_json::json!({"source": source, "name": name}))
        .collect();
    let config = serde_json::json!({"dir": dir, "out": out, "files": files, "tag": tag, "manifest": manifest, "tsc": tsc});
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

fn run() -> Result<(), Error> {
    let mut args = env::args().skip(1);
    let mode = args.next().ok_or(USAGE)?;
    if !["package", "library", "assemble"].contains(&mode.as_str()) {
        return Err(USAGE.into());
    }
    let dir = PathBuf::from(args.next().ok_or(USAGE)?).canonicalize()?;
    let args = parse(args)?;
    let out = args.out.ok_or(USAGE)?;
    std::fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let tsc = args.tsc.unwrap_or_else(|| "tsc".to_owned());
    let manifest = read_manifest(&dir)?;
    let library = manifest.library.clone().unwrap();
    match mode.as_str() {
        "package" => {
            let artifact = build_library(&dir, args.features.as_deref(), None)?;
            let prefix = if cfg!(windows) { "" } else { "lib" };
            let name = library_file(
                &library,
                prefix,
                &format!(".{}", env::consts::DLL_EXTENSION),
            );
            write_packages(&dir, &out, &manifest, &[(artifact, name)], "host", &tsc)
        }
        "library" => {
            let key = host_platform(args.zigbuild.as_deref())?;
            let (_, prefix, suffix, _) = PLATFORMS.iter().find(|p| p.0 == key).unwrap();
            require_plain(&library)?;
            let artifact = build_library(&dir, args.features.as_deref(), args.zigbuild.as_deref())?;
            let target = out.join(key);
            std::fs::create_dir_all(&target)?;
            let target = target.join(library_file(&library, prefix, suffix));
            std::fs::copy(&artifact, &target)?;
            println!("{}", target.display());
            Ok(())
        }
        _ => {
            let libraries = args.libraries.ok_or(USAGE)?;
            require_plain(&library)?;
            let mut files = Vec::new();
            for (key, prefix, suffix, _) in PLATFORMS.into_iter().filter(|&(.., shipped)| shipped) {
                let name = library_file(&library, prefix, suffix);
                let source = libraries.join(key).join(&name);
                if !source.is_file() {
                    return Err(format!(
                        "missing library for {key}: expected {}",
                        source.display()
                    )
                    .into());
                }
                files.push((source, name));
            }
            write_packages(&dir, &out, &manifest, &files, "platforms", &tsc)
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_musl_target_gets_the_musl_key_and_ships_no_outside_library() {
        assert_eq!(platform_of("linux", "x86_64", true), Some("linux-x64-musl"));
        assert_eq!(platform_of("linux", "aarch64", true), Some("linux-arm64-musl"));
        assert_eq!(platform_of("linux", "x86_64", false), Some("linux-x64"));
        assert_eq!(platform_of("macos", "aarch64", true), None);
        for key in ["linux-x64-musl", "linux-arm64-musl"] {
            assert!(PLATFORMS.iter().any(|p| p.0 == key && !p.3), "{key}");
        }
        // The key of a build follows its Linux target, whatever the host is.
        for (target, key) in [
            ("x86_64-unknown-linux-musl", "linux-x64-musl"),
            ("aarch64-unknown-linux-musl", "linux-arm64-musl"),
            ("x86_64-unknown-linux-gnu.2.28", "linux-x64"),
            ("aarch64-unknown-linux-gnu.2.28", "linux-arm64"),
        ] {
            assert_eq!(host_platform(Some(target)).ok(), Some(key), "{target}");
        }
    }
}
