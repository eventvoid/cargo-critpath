//! Reading the structure of a build out of `cargo build -v`.
//!
//! With `-v` Cargo prints one `` Running `…` `` line per piece of work it actually performs: a
//! `rustc` invocation for every dirty compilation unit, and a bare execution of the compiled
//! binary for every build script it runs. Those lines are the whole unit set and, through their
//! `--extern` arguments, the whole edge set.

use critpath_core::{EdgeKind, UnitKind};

use crate::error::ParseError;

/// What a `` Running `…` `` line was doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// A `rustc` invocation compiling one unit.
    Compile,
    /// Execution of an already-compiled build script.
    BuildScriptRun,
}

/// A package named exactly, by a source path that contains `name-version/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct PackageRef {
    /// The package name, as it appears in the registry path.
    pub name: String,
    /// The package version, as it appears in the registry path.
    pub version: String,
}

/// One `--extern` argument: a dependency, and what the consumer needs of it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExternRef {
    /// The name the consumer knows the dependency by.
    pub name: String,
    /// The artifact's file extension, kept for diagnostics.
    pub extension: String,
    /// What the consumer needs before it can start: `.rmeta` is metadata, anything else is a
    /// complete build.
    pub edge: EdgeKind,
    /// The producer's `-C extra-filename`, including its leading dash, taken from the artifact's
    /// file name. Edges resolve by this rather than by name, because names are ambiguous across
    /// versions and across host and target instances of one package.
    pub producer: Option<String>,
}

/// One piece of work Cargo performed, as the verbose log recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Invocation {
    /// Position among the `Running` lines, counted from zero. Cargo prints these as it starts each
    /// job, so the order approximates the order the timing report lists units in.
    pub position: usize,
    /// Whether this compiled something or ran a build script.
    pub role: Role,
    /// The `--crate-name`, or empty for a build-script execution.
    pub crate_name: String,
    /// Every `--crate-type`. Empty means a binary, which is what Cargo passes for a bare `bin`.
    pub crate_types: Vec<String>,
    /// The `--emit` list.
    pub emits: Vec<String>,
    /// Whether the unit emits metadata, which is exactly when a dependent may start against it
    /// before it has finished.
    pub pipelined: bool,
    /// The `-C extra-filename`, including its leading dash. This is the unit's identity: every
    /// `--extern` path that names this unit's artifact embeds it.
    pub extra_filename: Option<String>,
    /// The `-C metadata` hash, which distinguishes two instances of one package that differ only
    /// in features or in being built for the host rather than the target.
    pub metadata_hash: Option<String>,
    /// Every `--extern` that named an artifact. `--extern proc_macro`, which names the compiler's
    /// own crate and has no path, is not one of these.
    pub externs: Vec<ExternRef>,
    /// The source file the invocation compiled, exactly as it appeared. Cargo writes this relative
    /// to the workspace root for a workspace-local target and absolute for a registry one.
    pub source_path: String,
    /// The features the unit was compiled with, sorted, recovered from `--cfg feature="…"`.
    pub features: Vec<String>,
    /// The package, where the source path stated it exactly.
    pub registry_package: Option<PackageRef>,
    /// The package's own directory name, for a workspace-local target whose source path did not.
    /// A heuristic: a package need not be named after its directory.
    pub directory_package: Option<String>,
    /// The package name, for a build-script compilation or execution, taken from the
    /// `target/<profile>/build/<package>-<hash>` directory Cargo gives it. Exact.
    pub run_package: Option<String>,
    /// The compiled build script's `-C extra-filename`, including its leading dash, for a
    /// build-script execution. This ties the run to the compilation exactly.
    pub run_artifact: Option<String>,
    /// What sort of work this is.
    pub kind: UnitKind,
    /// Whether the unit needs its whole transitive dependency closure to be fully built, which is
    /// Cargo's `requires_upstream_objects`.
    pub requires_full_closure: bool,
}

/// Everything one verbose build log described.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerboseLog {
    /// The work Cargo performed, in the order it started it.
    pub invocations: Vec<Invocation>,
    /// `Running` lines that were neither a `rustc` invocation nor a build-script execution, kept
    /// verbatim so that they can be named if they turn out to matter.
    pub unrecognised: Vec<String>,
}

/// Reads every `` Running `…` `` line out of a verbose build log.
///
/// # Errors
///
/// [`ParseError::NotUtf8`] if the text holds NUL characters, which means it was decoded from
/// UTF-16 rather than the UTF-8 Cargo wrote; [`ParseError::NoInvocations`] if the log holds none,
/// which means it was captured without `-v` or from a build that had nothing to do; and
/// [`ParseError::UnbalancedQuoting`] if a line is cut off inside a quoted argument.
pub fn parse(text: &str) -> Result<VerboseLog, ParseError> {
    crate::error::reject_wide_text(text, "build log")?;
    let mut invocations = Vec::new();
    let mut unrecognised = Vec::new();

    for command in commands(text) {
        let argv = split_command(&command).ok_or_else(|| ParseError::UnbalancedQuoting {
            line: truncate(&command),
        })?;
        let position = invocations.len();
        match classify(&argv, position) {
            Some(invocation) => invocations.push(invocation),
            None => unrecognised.push(command),
        }
    }

    if invocations.is_empty() {
        return Err(ParseError::NoInvocations);
    }
    Ok(VerboseLog {
        invocations,
        unrecognised,
    })
}

/// The text between the backticks of every `Running` line.
///
/// A command normally occupies one line, but an argument may contain a newline, so a line whose
/// closing backtick is missing is continued into the following lines.
fn commands(text: &str) -> Vec<String> {
    const MARKER: &str = "Running `";
    let mut out = Vec::new();
    let mut pending: Option<String> = None;

    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match pending.take() {
            Some(mut open) => {
                open.push('\n');
                match line.strip_suffix('`') {
                    Some(rest) => {
                        open.push_str(rest);
                        out.push(open);
                    }
                    None => {
                        open.push_str(line);
                        pending = Some(open);
                    }
                }
            }
            None => {
                let trimmed = line.trim_start();
                let Some(rest) = trimmed.strip_prefix(MARKER) else {
                    continue;
                };
                match rest.strip_suffix('`') {
                    Some(command) => out.push(command.to_owned()),
                    None => pending = Some(rest.to_owned()),
                }
            }
        }
    }
    // A command left open at end of input is still worth reporting; the caller will fail on it.
    if let Some(open) = pending {
        out.push(open);
    }
    out
}

/// Splits a command into arguments, undoing exactly the escaping Cargo applied.
///
/// Cargo renders each argument through one escaper, and which one depends on the platform the log
/// was captured on rather than the one reading it. Both have to be undone, and the two disagree
/// about backslashes, so neither "a backslash escapes the next character" nor "a backslash is
/// literal" is right on its own:
///
/// - The Unix escaper wraps the argument in single quotes, and renders `'` as `'\''` and `!` as
///   `'\!'` — closing the quotes, escaping the character, reopening. So outside quotes a backslash
///   is meaningful, but only ever before one of those two characters.
/// - The Windows escaper wraps the argument in double quotes and doubles a backslash only where it
///   precedes a quote. So inside quotes a lone backslash is a path separator, not an escape.
///
/// Anything else is a literal backslash, which is what every path separator in a Windows log is.
///
/// Returns `None` if a quote is left open, because the alternative is to invent arguments.
fn split_command(command: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = command.chars();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if started {
                    args.push(core::mem::take(&mut current));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return None,
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // Only a quote or another backslash was escaped. Anything else is a
                        // separator in a Windows path and both characters are kept.
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(literal) => {
                                current.push('\\');
                                current.push(literal);
                            }
                            None => return None,
                        },
                        Some(inner) => current.push(inner),
                        None => return None,
                    }
                }
            }
            // Outside quotes only an apostrophe or an exclamation mark is ever escaped, those being
            // the two characters the Unix escaper closes the quotes around.
            '\\' => {
                started = true;
                match chars.next() {
                    Some(escaped @ ('\'' | '!')) => current.push(escaped),
                    Some(literal) => {
                        current.push('\\');
                        current.push(literal);
                    }
                    None => return None,
                }
            }
            other => {
                started = true;
                current.push(other);
            }
        }
    }
    if started {
        args.push(current);
    }
    Some(args)
}

/// Decides what a split command was, and reads out everything the graph needs.
fn classify(argv: &[String], position: usize) -> Option<Invocation> {
    if let Some(first) = argv.first() {
        if file_stem(first) == "build-script-build" {
            let (package, artifact) = split_build_directory(parent_of(first));
            return Some(Invocation {
                position,
                role: Role::BuildScriptRun,
                crate_name: String::new(),
                crate_types: Vec::new(),
                emits: Vec::new(),
                pipelined: false,
                extra_filename: None,
                metadata_hash: None,
                externs: Vec::new(),
                source_path: String::new(),
                features: Vec::new(),
                registry_package: None,
                directory_package: None,
                run_package: package,
                run_artifact: artifact,
                // A build-script execution is not a compilation. It needs its own executable, and
                // that executable is a binary, so it already waits for its own whole closure.
                // Requiring the closure again here would constrain nothing and would be wrong in
                // principle: running a program does not link anything.
                kind: UnitKind::BuildScriptRun,
                requires_full_closure: false,
            });
        }
    }

    let compiler = argv.iter().position(|a| file_stem(a) == "rustc")?;
    let args = argv.get(compiler.checked_add(1)?..)?;

    let crate_name = value_of(args, "--crate-name")?.to_owned();
    let crate_types: Vec<String> = values_of(args, "--crate-type");
    let emits: Vec<String> = value_of(args, "--emit")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let source_path = args
        .iter()
        .find(|a| a.ends_with(".rs"))
        .cloned()
        .unwrap_or_default();

    let codegen: Vec<&str> = values_of_borrowed(args, "-C");
    let extra_filename = codegen
        .iter()
        .find_map(|v| v.strip_prefix("extra-filename="))
        .map(str::to_owned);
    let metadata_hash = codegen
        .iter()
        .find_map(|v| v.strip_prefix("metadata="))
        .map(str::to_owned);

    let mut features: Vec<String> = values_of_borrowed(args, "--cfg")
        .iter()
        .filter_map(|v| {
            v.strip_prefix("feature=\"")
                .and_then(|v| v.strip_suffix('"'))
        })
        .map(str::to_owned)
        .collect();
    features.sort();
    features.dedup();

    let externs = values_of_borrowed(args, "--extern")
        .iter()
        .filter_map(|spec| extern_ref(spec))
        .collect();

    let out_dir = value_of(args, "--out-dir").unwrap_or_default();
    let (build_package, _) = split_build_directory(out_dir);
    // Cargo compiles a build script into `target/<profile>/build/<package>-<hash>` and everything
    // else into `target/<profile>/deps`, so the output directory identifies a build script exactly.
    // Its `--crate-name` does not: it is `build_script_build` only when the script is called
    // `build.rs`.
    let is_build_script = build_package.is_some();
    let has_test_harness = args.iter().any(|a| a == "--test" || a == "--bench");

    let (kind, requires_full_closure) = if is_build_script {
        (UnitKind::BuildScriptCompile, true)
    } else if has_test_harness {
        (UnitKind::Test, true)
    } else if crate_types.iter().any(|t| t == "proc-macro") {
        (UnitKind::ProcMacro, true)
    } else if crate_types.is_empty() || crate_types.iter().any(|t| t == "bin") {
        (UnitKind::Bin, true)
    } else if crate_types.iter().all(|t| t == "lib" || t == "rlib") {
        // Cargo's own rule, `mode.is_any_test() || !matches!(crate_type, Lib | Rlib)`: a plain
        // library is the only thing that can be produced from its dependencies' metadata alone.
        (UnitKind::Lib, false)
    } else {
        (UnitKind::Other, true)
    };

    Some(Invocation {
        position,
        role: Role::Compile,
        pipelined: emits.iter().any(|e| e == "metadata"),
        registry_package: registry_package(&source_path),
        directory_package: directory_package(&source_path),
        run_package: build_package,
        run_artifact: None,
        crate_name,
        crate_types,
        emits,
        extra_filename,
        metadata_hash,
        externs,
        source_path,
        features,
        kind,
        requires_full_closure,
    })
}

/// One `--extern name=path` argument. `--extern proc_macro` has no path and names the compiler's
/// own crate rather than a unit of this build, so it yields nothing.
fn extern_ref(spec: &str) -> Option<ExternRef> {
    let (name, path) = spec.split_once('=')?;
    let file = file_name(path);
    let (stem, extension) = match file.rsplit_once('.') {
        Some((stem, extension)) => (stem, extension),
        None => (file, ""),
    };
    Some(ExternRef {
        name: name.to_owned(),
        extension: extension.to_owned(),
        // `.rmeta` is the only artifact a consumer can start against before its producer has
        // finished. `.rlib`, `.dylib`, `.so` and anything unfamiliar all mean a complete build,
        // which is the conservative reading.
        edge: if extension == "rmeta" {
            EdgeKind::Metadata
        } else {
            EdgeKind::Full
        },
        producer: trailing_hash(stem).map(str::to_owned),
    })
}

/// `target/<profile>/build/<package>-<hash>` split into its package name and the hash, which is the
/// compiled build script's own `-C extra-filename`.
fn split_build_directory(dir: &str) -> (Option<String>, Option<String>) {
    if file_name(parent_of(dir)) != "build" {
        return (None, None);
    }
    let leaf = file_name(dir);
    match trailing_hash(leaf) {
        Some(hash) => {
            let name = leaf.get(..leaf.len().saturating_sub(hash.len()));
            (name.map(str::to_owned), Some(hash.to_owned()))
        }
        None => (None, None),
    }
}

/// The `-HASH` suffix Cargo appends to an artifact's file stem, if there is one.
fn trailing_hash(stem: &str) -> Option<&str> {
    let dash = stem.rfind('-')?;
    let hash = stem.get(dash..)?;
    let digits = hash.get(1..)?;
    if digits.len() >= 8 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash)
    } else {
        None
    }
}

/// The package a registry or git source path names: `…/name-1.2.3/src/lib.rs`.
fn registry_package(source: &str) -> Option<PackageRef> {
    for component in components(source) {
        let Some(dash) = component.rfind('-') else {
            continue;
        };
        let (name, version) = (
            component.get(..dash)?,
            component.get(dash.saturating_add(1)..)?,
        );
        if name.is_empty() || !looks_like_version(version) {
            continue;
        }
        return Some(PackageRef {
            name: name.to_owned(),
            version: version.to_owned(),
        });
    }
    None
}

/// Whether a path component's tail is a version: three dot-separated numbers, then anything.
fn looks_like_version(text: &str) -> bool {
    let mut parts = text.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let patch_number = patch.split(['-', '+']).next().unwrap_or_default();
    numeric(major) && numeric(minor) && numeric(patch_number)
}

/// The directory a workspace-local target's package most likely lives in: the source path with any
/// `src`, `bin`, `examples`, `tests` or `benches` component stripped from its tail.
fn directory_package(source: &str) -> Option<String> {
    let mut parts: Vec<&str> = components(parent_of(source)).collect();
    while matches!(
        parts.last().copied(),
        Some("src" | "bin" | "examples" | "tests" | "benches")
    ) {
        parts.pop();
    }
    parts.last().map(|s| (*s).to_owned())
}

/// A path's components, whichever separator it uses. Cargo writes native separators, and a log
/// captured on Windows is analysed on Windows, but a fixture is read on every platform.
fn components(path: &str) -> impl DoubleEndedIterator<Item = &str> {
    // A slice rather than an array: only the slice form searches backwards on the pinned minimum
    // supported Rust version.
    const SEPARATORS: &[char] = &['/', '\\'];
    path.split(SEPARATORS).filter(|s| !s.is_empty())
}

fn file_name(path: &str) -> &str {
    components(path).next_back().unwrap_or("")
}

fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    name.strip_suffix(".exe").unwrap_or(name)
}

fn parent_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(at) => path.get(..at).unwrap_or(""),
        None => "",
    }
}

/// The value of `--name value` or `--name=value`, whichever form was used.
fn value_of<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    values_of_borrowed(args, name).first().copied()
}

fn values_of(args: &[String], name: &str) -> Vec<String> {
    values_of_borrowed(args, name)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn values_of_borrowed<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut expecting = false;
    for arg in args {
        if expecting {
            out.push(arg.as_str());
            expecting = false;
            continue;
        }
        if arg == name {
            expecting = true;
        } else if let Some(rest) = arg.strip_prefix(name) {
            if let Some(value) = rest.strip_prefix('=') {
                out.push(value);
            } else if name.len() == 2 && !rest.is_empty() {
                // Short options such as `-Cmetadata=…` may be written without a separator.
                out.push(rest);
            }
        }
    }
    out
}

fn truncate(line: &str) -> String {
    const LIMIT: usize = 120;
    match line.char_indices().nth(LIMIT) {
        Some((at, _)) => match line.get(..at) {
            Some(head) => format!("{head}…"),
            None => line.to_owned(),
        },
        None => line.to_owned(),
    }
}
