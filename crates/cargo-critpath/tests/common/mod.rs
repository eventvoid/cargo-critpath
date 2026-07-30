//! Shared plumbing for the command-line tests: run the tool in process and capture both streams.

#![allow(dead_code, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use cargo_critpath::cli::{self, Outcome};

/// What one invocation of the tool produced.
pub struct Run {
    pub outcome: Outcome,
    pub out: String,
    pub err: String,
}

impl Run {
    /// The exit code the binary would return.
    pub fn code(&self) -> i32 {
        self.outcome.code()
    }
}

/// Runs the tool with `args`, as Cargo would after the binary name.
pub fn run(args: &[&str]) -> Run {
    let mut argv = vec!["cargo-critpath".to_owned()];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let outcome = cli::run(argv, &mut out, &mut err);
    Run {
        outcome,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

/// The path of a file under `tests/fixtures`.
pub fn fixture(relative: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
        .to_string_lossy()
        .into_owned()
}

/// The arguments that analyse the captured workspace build.
pub fn workspace() -> Vec<String> {
    vec![
        "--log".to_owned(),
        fixture("workspace/verbose.log"),
        "--timings".to_owned(),
        fixture("workspace/cargo-timing.html"),
        "--metadata".to_owned(),
        fixture("workspace/cargo-metadata.json"),
    ]
}

/// Runs the tool over the captured workspace build with `extra` appended.
pub fn run_workspace(extra: &[&str]) -> Run {
    let mut args = workspace();
    args.extend(extra.iter().map(|a| (*a).to_owned()));
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&borrowed)
}

/// The path of a golden file under `tests/golden`.
pub fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compares output against a committed golden file, printing the command to regenerate it.
///
/// Line endings are normalised on the way in, because a checkout that translates them would
/// otherwise fail this test on Windows for a reason that has nothing to do with the tool.
pub fn assert_golden(name: &str, actual: &str, regenerate: &str) {
    let path = golden(name);
    let expected = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "output no longer matches tests/golden/{name}.\n\
         If the change is deliberate, regenerate it with:\n  {regenerate}\n\
         --- expected ---\n{expected}\n--- actual ---\n{actual}"
    );
}

/// Writes raw bytes to a uniquely named file under the temporary directory and returns its path.
///
/// Separate from [`scratch_file`] because an encoding that is not UTF-8 cannot be held in a `str`.
pub fn scratch_bytes(name: &str, contents: &[u8]) -> String {
    let directory =
        std::env::temp_dir().join(format!("cargo-critpath-tests-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

/// Writes `contents` to a uniquely named file under the temporary directory and returns its path.
pub fn scratch_file(name: &str, contents: &str) -> String {
    let directory =
        std::env::temp_dir().join(format!("cargo-critpath-tests-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}
