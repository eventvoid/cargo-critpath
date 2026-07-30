//! Getting the three inputs off disk, or out of Cargo.
//!
//! Everything else in this crate takes text, so this module is the whole of its contact with the
//! filesystem and with other processes. Keeping it separate is what lets the parsers be tested
//! against fixtures rather than against a machine.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The text of one build's outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuildOutputs {
    /// Cargo's stderr from a `cargo build --timings -v` run.
    pub verbose_log: String,
    /// The contents of `cargo-timing.html`.
    pub timing_html: String,
    /// `cargo metadata --format-version 1` output, if it was available.
    pub cargo_metadata: Option<String>,
}

/// The three files this crate reads, named as they are conventionally captured.
pub const VERBOSE_LOG_NAME: &str = "verbose.log";
/// The timing report Cargo writes, and the name it is conventionally captured under.
pub const TIMING_REPORT_NAME: &str = "cargo-timing.html";
/// `cargo metadata` output, under the name this crate looks for.
pub const METADATA_NAME: &str = "cargo-metadata.json";

/// Reads a directory holding a captured build: `verbose.log`, `cargo-timing.html`, and optionally
/// `cargo-metadata.json`.
///
/// # Errors
///
/// Any [`io::Error`] from reading the two required files. A missing `cargo-metadata.json` is not an
/// error; workspace-local packages are then attributed by directory name instead.
pub fn from_directory(directory: &Path) -> io::Result<BuildOutputs> {
    let metadata_path = directory.join(METADATA_NAME);
    Ok(BuildOutputs {
        verbose_log: read_text(&directory.join(VERBOSE_LOG_NAME))?,
        timing_html: read_text(&directory.join(TIMING_REPORT_NAME))?,
        cargo_metadata: read_text(&metadata_path).ok(),
    })
}

/// Reads the three inputs from paths given individually.
///
/// # Errors
///
/// Any [`io::Error`] from reading a named file. Unlike [`from_directory`], a metadata path that was
/// named and cannot be read is an error, because the caller asked for it.
pub fn from_paths(
    verbose_log: &Path,
    timing_report: &Path,
    cargo_metadata: Option<&Path>,
) -> io::Result<BuildOutputs> {
    Ok(BuildOutputs {
        verbose_log: read_text(verbose_log)?,
        timing_html: read_text(timing_report)?,
        cargo_metadata: match cargo_metadata {
            Some(path) => Some(read_text(path)?),
            None => None,
        },
    })
}

/// The timing report Cargo wrote most recently under a target directory.
///
/// Cargo writes `cargo-timings/cargo-timing.html` and also keeps a dated copy per build. The
/// undated name is the current one, so it is preferred; the dated copies are searched only if it is
/// absent.
///
/// # Errors
///
/// [`io::ErrorKind::NotFound`] if the directory holds no timing report, with a message saying which
/// command produces one.
pub fn latest_timing_report(target_directory: &Path) -> io::Result<PathBuf> {
    let timings = target_directory.join("cargo-timings");
    let current = timings.join(TIMING_REPORT_NAME);
    if current.is_file() {
        return Ok(current);
    }

    let mut dated: Vec<PathBuf> = fs::read_dir(&timings)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension() == Some(OsStr::new("html"))
                        && path
                            .file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(|name| name.starts_with("cargo-timing"))
                })
                .collect()
        })
        .unwrap_or_default();
    // Cargo's dated names sort chronologically, and sorting keeps the choice reproducible.
    dated.sort();

    dated.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no timing report under {}; produce one with \
                 `cargo build --timings -v --offline 2>build.log`",
                timings.display()
            ),
        )
    })
}

/// Runs `cargo metadata --format-version 1 --offline --no-deps` for a manifest and returns its
/// output.
///
/// Only the workspace's own packages are needed, because a registry crate names itself in its
/// source path, and `--no-deps` avoids touching the network or the registry index.
///
/// # Errors
///
/// Any [`io::Error`] from spawning Cargo, and an error carrying Cargo's own stderr if it exits
/// unsuccessfully.
pub fn run_cargo_metadata(manifest_path: &Path) -> io::Result<String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--offline")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(manifest_path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "`cargo metadata` failed for {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Reads a file as UTF-8 text, naming the encoding when it is not.
///
/// # Errors
///
/// Any [`io::Error`] from reading the file, and [`io::ErrorKind::InvalidData`] when the contents are
/// not UTF-8.
///
/// A byte-order mark means the file is UTF-16, which is what a redirection under PowerShell writes.
/// `read_to_string` would reject it as invalid UTF-8 and leave the reader checking the path and the
/// build, neither of which is the problem.
pub fn read_text(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let wide = matches!(bytes.get(..2), Some([0xFF, 0xFE] | [0xFE, 0xFF]));
    if wide {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} begins with a UTF-16 byte-order mark, but Cargo writes UTF-8; \
                 a redirection under PowerShell writes UTF-16. Capture it with \
                 `cargo build --timings -v --offline 2>&1 | Out-File -Encoding utf8 build.log`, \
                 or from a shell that redirects as UTF-8",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not UTF-8 text, which is what Cargo writes ({}); \
                 capture it again with `cargo build --timings -v --offline 2>build.log`, \
                 and check that nothing has rewritten or truncated the file since",
                path.display(),
                error.utf8_error()
            ),
        )
    })
}
