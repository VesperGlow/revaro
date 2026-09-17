//! Build orchestration for the Revaro workspace.
//!
//! Replacing the old two-toolchain setup (npm + Vite for the UI, Go for the
//! server) with a single driver means there is one place that knows how to turn
//! the sources into runnable artifacts. Everything runs through `cargo`; the
//! frontend is the only part that needs an extra step, because a
//! `wasm32-unknown-unknown` build still needs `wasm-bindgen` to emit JavaScript
//! glue.
//!
//! ```text
//! cargo xtask web-build     build the Leptos client into dist/web
//! cargo xtask web-check     type-check the client for wasm32
//! cargo xtask check         fmt + clippy + tests for the whole workspace
//! cargo xtask build         release server binary plus web bundle
//! ```
//!
//! Usage failures exit with code 2 so CI can distinguish "you asked for
//! something impossible" from "the build failed".

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The wasm target the browser bundle is compiled for.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Crate that produces the browser bundle.
const WEB_PACKAGE: &str = "revaro-web";

/// Version of the `wasm-bindgen` CLI the bundle must be generated with.
///
/// This has to match the `wasm-bindgen` crate pinned in the workspace manifest
/// exactly; the generated glue and the compiled module share a private ABI
/// version. `xtask` is dependency-free so the pin is duplicated here on
/// purpose, and `cargo xtask check` fails loudly if the two drift apart.
const WASM_BINDGEN_CLI_VERSION: &str = "0.2.128";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(task) = args.next() else {
        print_usage();
        return ExitCode::from(2);
    };
    let rest: Vec<String> = args.collect();

    let root = match workspace_root() {
        Ok(root) => root,
        Err(message) => {
            eprintln!("xtask: {message}");
            return ExitCode::FAILURE;
        }
    };

    let result = match task.as_str() {
        "web-build" => web_build(&root, &rest),
        "web-check" => web_check(&root),
        "check" => check(&root),
        "build" => build(&root),
        "clean" => clean(&root),
        "-h" | "--help" | "help" => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("xtask: unknown task {other:?}\n");
            print_usage();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Revaro build driver\n\n\
         USAGE:\n    cargo xtask <task>\n\n\
         TASKS:\n\
         \x20   web-build     Build the Leptos client and emit the wasm bundle to dist/web\n\
         \x20   web-check     Type-check the Leptos client for the wasm target only\n\
         \x20   check         cargo fmt --check, clippy and tests across the workspace\n\
         \x20   build         Release build of the server plus the web bundle\n\
         \x20   clean         Remove generated artifacts (dist/ and target/)\n"
    );
}

/// The workspace root, derived from this crate's manifest directory.
fn workspace_root() -> Result<PathBuf, String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "could not find the workspace root above {}",
            manifest.display()
        )
    })
}

/// Directory the browser bundle is emitted into.
fn dist_dir(root: &Path) -> PathBuf {
    root.join("dist").join("web")
}

fn web_build(root: &Path, args: &[String]) -> Result<(), String> {
    let release = !args.iter().any(|arg| arg == "--debug");
    let profile = if release { "release" } else { "debug" };

    let mut build = cargo();
    build
        .current_dir(root)
        .args(["build", "-p", WEB_PACKAGE, "--target", WASM_TARGET]);
    if release {
        build.arg("--release");
    }
    run(&mut build, "compile the web client for wasm32")?;

    let artifact = root
        .join("target")
        .join(WASM_TARGET)
        .join(profile)
        .join("revaro_web.wasm");
    if !artifact.is_file() {
        return Err(format!("expected wasm artifact at {}", artifact.display()));
    }

    let dist = dist_dir(root);
    // Start from an empty directory: `copy_static_assets` only adds and
    // overwrites, so a stylesheet deleted from `static/` would otherwise keep
    // being served from a stale `dist/web` copy.
    if dist.is_dir() {
        std::fs::remove_dir_all(&dist)
            .map_err(|error| format!("could not clear {}: {error}", dist.display()))?;
    }
    std::fs::create_dir_all(&dist)
        .map_err(|error| format!("could not create {}: {error}", dist.display()))?;

    // `--target web` emits an ES module plus a matched .wasm, which is exactly
    // what a plain `<script type="module">` can import — no bundler involved.
    let mut bindgen = Command::new(wasm_bindgen_binary());
    bindgen
        .current_dir(root)
        .arg("--target")
        .arg("web")
        .arg("--out-dir")
        .arg(&dist)
        .arg("--out-name")
        .arg("revaro_web")
        .arg("--no-typescript")
        .arg(&artifact);
    if let Err(message) = run(&mut bindgen, "run wasm-bindgen") {
        return Err(format!(
            "{message}\n\nInstall the matching CLI once with:\n    \
             cargo install wasm-bindgen-cli --version {WASM_BINDGEN_CLI_VERSION}"
        ));
    }

    copy_static_assets(root, &dist)?;
    println!("web bundle written to {}", dist.display());
    Ok(())
}

/// Locate the `wasm-bindgen` CLI, preferring a copy next to the cargo binary.
fn wasm_bindgen_binary() -> PathBuf {
    if let Ok(explicit) = std::env::var("WASM_BINDGEN") {
        return PathBuf::from(explicit);
    }
    PathBuf::from("wasm-bindgen")
}

/// Copy hand-written static files (index.html, icons, fonts) next to the bundle.
fn copy_static_assets(root: &Path, dist: &Path) -> Result<(), String> {
    let source = root.join("crates").join(WEB_PACKAGE).join("static");
    if !source.is_dir() {
        return Ok(());
    }
    copy_tree(&source, dist)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not read {}: {error}", source.display()))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not stat {}: {error}", from.display()))?;
        if file_type.is_dir() {
            std::fs::create_dir_all(&to)
                .map_err(|error| format!("could not create {}: {error}", to.display()))?;
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|error| format!("could not copy {}: {error}", from.display()))?;
            // `fs::copy` preserves the source permissions, so a restrictive
            // umask would leave the bundle unreadable to the user the server
            // runs as. Bundle files are public assets.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o644)).map_err(
                    |error| format!("could not set permissions on {}: {error}", to.display()),
                )?;
            }
        }
    }
    Ok(())
}

fn web_check(root: &Path) -> Result<(), String> {
    let mut command = cargo();
    command.current_dir(root).args([
        "check",
        "-p",
        WEB_PACKAGE,
        "--target",
        WASM_TARGET,
        "--all-targets",
    ]);
    run(&mut command, "type-check the web client")
}

/// Fail when the pinned `wasm-bindgen` crate and the CLI version drift apart.
///
/// `wasm-bindgen` glue and the compiled module share a private ABI version, so a
/// mismatch produces confusing runtime failures rather than a build error. The
/// pin is necessarily written down twice (the workspace manifest and this
/// driver), so it is checked rather than trusted.
fn check_wasm_bindgen_pin(root: &Path) -> Result<(), String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("could not read {}: {error}", manifest.display()))?;
    let pinned = manifest_dependency_version(&text, "wasm-bindgen").ok_or_else(|| {
        format!(
            "{} does not pin a `wasm-bindgen` version",
            manifest.display()
        )
    })?;
    let pinned = pinned.trim_start_matches('=');
    if pinned != WASM_BINDGEN_CLI_VERSION {
        return Err(format!(
            "wasm-bindgen version drift: Cargo.toml pins {pinned}, but xtask expects \
             {WASM_BINDGEN_CLI_VERSION}.\nUpdate WASM_BINDGEN_CLI_VERSION in xtask/src/main.rs \
             and install the matching CLI:\n    cargo install wasm-bindgen-cli --version {pinned}"
        ));
    }
    Ok(())
}

/// Extract the version of `name` from a manifest's `[workspace.dependencies]`
/// table, handling both `name = "1.2.3"` and `name = { version = "1.2.3", .. }`.
fn manifest_dependency_version(manifest: &str, name: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(name) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim();
        if let Some(inline) = rest.strip_prefix('{') {
            let version = inline
                .split(',')
                .map(str::trim)
                .find_map(|entry| entry.strip_prefix("version"))?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .trim_matches('"');
            return Some(version.to_owned());
        }
        return Some(rest.trim_matches('"').to_owned());
    }
    None
}

fn check(root: &Path) -> Result<(), String> {
    check_wasm_bindgen_pin(root)?;

    let mut fmt = cargo();
    fmt.current_dir(root)
        .args(["fmt", "--all", "--", "--check"]);
    run(&mut fmt, "check formatting")?;

    let mut clippy = cargo();
    clippy.current_dir(root).args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]);
    run(&mut clippy, "run clippy")?;

    let mut test = cargo();
    test.current_dir(root).args(["test", "--workspace"]);
    run(&mut test, "run the test suite")?;

    web_check(root)
}

fn build(root: &Path) -> Result<(), String> {
    web_build(root, &[])?;
    let mut command = cargo();
    command
        .current_dir(root)
        .args(["build", "--release", "-p", "revaro-server"]);
    run(&mut command, "build the server")
}

fn clean(root: &Path) -> Result<(), String> {
    let dist = root.join("dist");
    if dist.exists() {
        std::fs::remove_dir_all(&dist)
            .map_err(|error| format!("could not remove {}: {error}", dist.display()))?;
    }
    let mut command = cargo();
    command.current_dir(root).arg("clean");
    run(&mut command, "clean the cargo target directory")
}

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
}

/// Run a child process, turning a non-zero exit into a readable message.
fn run(command: &mut Command, description: &str) -> Result<(), String> {
    let printable = format!("{command:?}");
    let status = command
        .status()
        .map_err(|error| format!("could not {description} ({printable}): {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to {description}: {printable} exited with {status}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_plain_version_string() {
        let manifest = "[workspace.dependencies]\nwasm-bindgen = \"=0.2.128\"\n";
        assert_eq!(
            manifest_dependency_version(manifest, "wasm-bindgen").as_deref(),
            Some("=0.2.128")
        );
    }

    #[test]
    fn extracts_a_version_from_an_inline_table() {
        let manifest = "[workspace.dependencies]\nwasm-bindgen = { version = \"=0.2.128\", features = [\"x\"] }\n";
        assert_eq!(
            manifest_dependency_version(manifest, "wasm-bindgen").as_deref(),
            Some("=0.2.128")
        );
    }

    #[test]
    fn does_not_confuse_a_prefixed_dependency_name() {
        // `wasm-bindgen-futures` must not be mistaken for `wasm-bindgen`.
        let manifest = "[workspace.dependencies]\nwasm-bindgen-futures = \"=0.4.78\"\n";
        assert_eq!(manifest_dependency_version(manifest, "wasm-bindgen"), None);
    }

    #[test]
    fn reports_a_missing_dependency() {
        assert_eq!(
            manifest_dependency_version("[dependencies]\nserde = \"1\"\n", "wasm-bindgen"),
            None
        );
    }

    #[test]
    fn the_pin_agrees_with_this_driver() {
        // Guards the real manifest: if this fails, update WASM_BINDGEN_CLI_VERSION
        // and reinstall the CLI.
        let root = workspace_root().expect("workspace root");
        check_wasm_bindgen_pin(&root).expect("wasm-bindgen pin must match xtask");
    }
}
