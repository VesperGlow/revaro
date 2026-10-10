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

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest as _, Sha256};

/// The wasm target the browser bundle is compiled for.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Crate that produces the browser bundle.
const WEB_PACKAGE: &str = "revaro-web";

/// Version of the `wasm-bindgen` CLI the bundle must be generated with.
///
/// This has to match the `wasm-bindgen` crate pinned in the workspace manifest
/// exactly; the generated glue and the compiled module share a private ABI
/// version. The pin is duplicated here on purpose, and `cargo xtask check`
/// fails loudly if the two drift apart.
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

    // Assemble a complete bundle beside the live directory. The existing
    // instance keeps serving its matched JS/WASM while wasm-bindgen runs.
    let published_dist = args
        .windows(2)
        .find(|pair| pair[0] == "--out-dir")
        .map(|pair| root.join(&pair[1]))
        .unwrap_or_else(|| dist_dir(root));
    let dist = published_dist.with_file_name(format!("web-build-{}", std::process::id()));
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
    fingerprint_bundle(&dist)?;
    // Old HTML or an in-flight module import can still reference the previous
    // hashes during a rebuild. Keep those immutable URLs available.
    preserve_hashed_assets(&published_dist, &dist)?;
    let previous = published_dist.with_file_name(format!("web-previous-{}", std::process::id()));
    let had_previous = published_dist.is_dir();
    if had_previous {
        std::fs::rename(&published_dist, &previous)
            .map_err(|error| format!("could not preserve live web bundle: {error}"))?;
    }
    if let Err(error) = std::fs::rename(&dist, &published_dist) {
        if had_previous {
            let _ = std::fs::rename(&previous, &published_dist);
        }
        return Err(format!("could not publish web bundle: {error}"));
    }
    if had_previous {
        std::fs::remove_dir_all(&previous)
            .map_err(|error| format!("could not remove previous web bundle: {error}"))?;
    }
    println!("web bundle written to {}", published_dist.display());
    Ok(())
}

/// Name the paired WASM/JS by their contents and precompress the WASM once.
fn fingerprint_bundle(dist: &Path) -> Result<(), String> {
    let wasm_path = dist.join("revaro_web_bg.wasm");
    let wasm = std::fs::read(&wasm_path).map_err(|error| format!("read WASM: {error}"))?;
    let wasm_name = format!("core.{:x}.wasm", Sha256::digest(&wasm));
    std::fs::rename(&wasm_path, dist.join(&wasm_name))
        .map_err(|error| format!("fingerprint WASM: {error}"))?;

    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 11, 22);
        writer
            .write_all(&wasm)
            .map_err(|error| format!("compress WASM with Brotli: {error}"))?;
    }
    std::fs::write(dist.join(format!("{wasm_name}.br")), &compressed)
        .map_err(|error| format!("write Brotli WASM: {error}"))?;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gzip.write_all(&wasm)
        .map_err(|error| format!("compress WASM with gzip: {error}"))?;
    let gzip = gzip
        .finish()
        .map_err(|error| format!("finish gzip WASM: {error}"))?;
    std::fs::write(dist.join(format!("{wasm_name}.gz")), &gzip)
        .map_err(|error| format!("write gzip WASM: {error}"))?;

    let glue_path = dist.join("revaro_web.js");
    let glue =
        std::fs::read_to_string(&glue_path).map_err(|error| format!("read WASM glue: {error}"))?;
    if !glue.contains("new URL('revaro_web_bg.wasm', import.meta.url)") {
        return Err("wasm-bindgen default WASM URL changed; update fingerprinting".to_owned());
    }
    let glue = glue.replace("revaro_web_bg.wasm", &wasm_name);
    let glue_name = format!("revaro_web.{:x}.js", Sha256::digest(glue.as_bytes()));
    std::fs::write(dist.join(&glue_name), glue)
        .map_err(|error| format!("write fingerprinted WASM glue: {error}"))?;
    std::fs::remove_file(&glue_path)
        .map_err(|error| format!("remove unfingerprinted WASM glue: {error}"))?;

    let index_path = dist.join("index.html");
    let index = std::fs::read_to_string(&index_path)
        .map_err(|error| format!("read HTML shell: {error}"))?;
    if !index.contains("__REVARO_WASM__") || !index.contains("__REVARO_MODULE__") {
        return Err("HTML shell is missing bundle URL placeholders".to_owned());
    }
    std::fs::write(
        index_path,
        index
            .replace("__REVARO_WASM__", &wasm_name)
            .replace("__REVARO_MODULE__", &glue_name),
    )
    .map_err(|error| format!("write HTML bundle URLs: {error}"))?;
    println!(
        "WASM: {} bytes, Brotli: {} bytes, gzip: {} bytes ({wasm_name})",
        wasm.len(),
        compressed.len(),
        gzip.len()
    );
    Ok(())
}

fn preserve_hashed_assets(previous: &Path, dist: &Path) -> Result<(), String> {
    if !previous.is_dir() {
        return Ok(());
    }
    for entry in
        std::fs::read_dir(previous).map_err(|error| format!("read previous bundle: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read previous asset: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("core.")
            || (name.starts_with("revaro_web.") && name != "revaro_web.js"))
            && entry.path().is_file()
            && !dist.join(name.as_ref()).exists()
        {
            std::fs::copy(entry.path(), dist.join(name.as_ref()))
                .map_err(|error| format!("preserve previous hashed asset: {error}"))?;
        }
    }
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
    copy_tree(&source, dist)?;
    // Emit a classic service worker for Firefox as well as Chromium/Edge.
    // Both worker and ESM upload adapter are generated from one policy source.
    let core = std::fs::read_to_string(source.join("transport-core.js"))
        .map_err(|error| format!("read shared transport policy: {error}"))?;
    let worker = std::fs::read_to_string(source.join("transport-sw.js"))
        .map_err(|error| format!("read transport worker: {error}"))?;
    let import = "import { fileResponse, bufferedRequest, configureTransport, clearTransportCache, setPlaybackState, isFileResource, usesNativeFileLoading } from './transport-core.js';\n";
    let worker = worker.strip_prefix(import).ok_or_else(|| {
        "transport worker imports changed; update classic worker bundling".to_owned()
    })?;
    if core.lines().any(|line| line.starts_with("import ")) {
        return Err(
            "shared transport policy imports require updating classic worker bundling".into(),
        );
    }
    let core = core
        .lines()
        .map(|line| line.strip_prefix("export ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dist.join("transport-worker.js"), format!(
        "// Generated from transport-core.js + transport-sw.js by cargo xtask web-build.\n{core}\n{worker}"
    )).map_err(|error| format!("write classic transport worker: {error}"))?;
    Ok(())
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
    fn bundle_hashes_match_contents_and_compression_round_trips() {
        use std::io::Read as _;

        let dist = std::env::temp_dir().join(format!("revaro-bundle-test-{}", std::process::id()));
        std::fs::create_dir_all(&dist).unwrap();
        let wasm = b"\0asm\x01\0\0\0";
        std::fs::write(dist.join("revaro_web_bg.wasm"), wasm).unwrap();
        std::fs::write(
            dist.join("revaro_web.js"),
            "const path = new URL('revaro_web_bg.wasm', import.meta.url);",
        )
        .unwrap();
        std::fs::write(
            dist.join("index.html"),
            "<link href='/__REVARO_WASM__'><script src='/__REVARO_MODULE__'></script>",
        )
        .unwrap();
        fingerprint_bundle(&dist).unwrap();
        let wasm_name = format!("core.{:x}.wasm", Sha256::digest(wasm));
        let index = std::fs::read_to_string(dist.join("index.html")).unwrap();
        assert!(index.contains(&wasm_name));
        assert!(!index.contains("__REVARO_"));
        let module_name = index
            .split("<script src='/")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let module = std::fs::read(dist.join(module_name)).unwrap();
        assert_eq!(
            module_name,
            format!("revaro_web.{:x}.js", Sha256::digest(&module))
        );
        assert!(String::from_utf8(module).unwrap().contains(&wasm_name));
        for encoding in ["br", "gz"] {
            let compressed = std::fs::read(dist.join(format!("{wasm_name}.{encoding}"))).unwrap();
            let mut decoded = Vec::new();
            if encoding == "br" {
                brotli::Decompressor::new(compressed.as_slice(), 4096)
                    .read_to_end(&mut decoded)
                    .unwrap();
            } else {
                flate2::read::GzDecoder::new(compressed.as_slice())
                    .read_to_end(&mut decoded)
                    .unwrap();
            }
            assert_eq!(decoded, wasm);
        }
        let next = dist.with_file_name(format!("revaro-bundle-next-{}", std::process::id()));
        std::fs::create_dir_all(&next).unwrap();
        preserve_hashed_assets(&dist, &next).unwrap();
        assert_eq!(std::fs::read(next.join(&wasm_name)).unwrap(), wasm);
        assert!(next.join(module_name).is_file());
        assert!(!next.join("index.html").exists());
        std::fs::remove_dir_all(dist).unwrap();
        std::fs::remove_dir_all(next).unwrap();
    }

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
