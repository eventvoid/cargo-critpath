//! Attributing a source path to a package, exactly, using `cargo metadata`.
//!
//! A `rustc` invocation identifies its unit by a source path. For a registry crate that path
//! contains `name-version/` and says which package it is. For a workspace-local crate it does not,
//! and guessing from the directory name is wrong often enough to matter: `bevy` keeps several
//! distinct procedural-macro packages in directories all named `macros`, and naming them after
//! their directory collapses six units onto one key.
//!
//! `cargo metadata --format-version 1` gives every package's `manifest_path`. A source path belongs
//! to the package whose manifest directory is its longest matching prefix, which is exact and needs
//! no heuristic at all.

use serde_json::Value;

use crate::error::ParseError;

/// Every package a workspace contains, indexed by the directory its manifest lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageIndex {
    /// The workspace root, used to make a relative source path absolute. Cargo writes a
    /// workspace-local target's source path relative to it.
    workspace_root: String,
    /// `(manifest directory, package name, package version)`, longest directory first, so the first
    /// match is the longest one.
    packages: Vec<(String, String, String)>,
}

impl PackageIndex {
    /// How many packages the index holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Whether the index holds no packages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// The package a source path belongs to, by longest-prefix match on the manifest directories.
    ///
    /// ```
    /// # use cargo_critpath::parse_cargo_metadata;
    /// let metadata = r#"{"workspace_root":"/w","packages":[
    ///     {"name":"app","version":"0.1.0","manifest_path":"/w/app/Cargo.toml"},
    ///     {"name":"leaf","version":"0.1.0","manifest_path":"/w/leaf/Cargo.toml"}]}"#;
    /// let index = parse_cargo_metadata(metadata)?;
    /// assert_eq!(index.package_at("app/src/main.rs"), Some(("app", "0.1.0")));
    /// assert_eq!(index.package_at("/w/leaf/build.rs"), Some(("leaf", "0.1.0")));
    /// assert_eq!(index.package_at("elsewhere/src/lib.rs"), None);
    /// assert_eq!(index.package_at(""), None);
    /// # Ok::<(), cargo_critpath::ParseError>(())
    /// ```
    #[must_use]
    pub fn package_at(&self, source_path: &str) -> Option<(&str, &str)> {
        // An empty path is not the workspace root; a build-script execution has no source file, and
        // attributing it to whichever package sits at the root would be a confident wrong answer.
        if source_path.is_empty() {
            return None;
        }
        let normalised = normalise(source_path);
        let absolute = if is_absolute(&normalised) {
            normalised
        } else {
            let root = normalise(&self.workspace_root);
            format!("{}/{}", root.trim_end_matches('/'), normalised)
        };
        self.packages
            .iter()
            .find(|(directory, _, _)| under(&absolute, directory))
            .map(|(_, name, version)| (name.as_str(), version.as_str()))
    }
}

/// Reads `cargo metadata --format-version 1` output.
///
/// Only `workspace_root` and each package's `name`, `version` and `manifest_path` are read; every
/// other field is ignored, so a newer Cargo does not break this.
///
/// # Errors
///
/// [`ParseError::Metadata`] if the text is not JSON, or does not have the shape `cargo metadata`
/// produces.
pub fn parse(text: &str) -> Result<PackageIndex, ParseError> {
    let value: Value = serde_json::from_str(text).map_err(|e| ParseError::Metadata {
        detail: e.to_string(),
    })?;
    let packages = value
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| ParseError::Metadata {
            detail: "there is no `packages` array".to_owned(),
        })?;

    let mut entries: Vec<(String, String, String)> = Vec::with_capacity(packages.len());
    for package in packages {
        let (Some(name), Some(version), Some(manifest)) = (
            package.get("name").and_then(Value::as_str),
            package.get("version").and_then(Value::as_str),
            package.get("manifest_path").and_then(Value::as_str),
        ) else {
            continue;
        };
        entries.push((
            normalise(parent_of(manifest)),
            name.to_owned(),
            version.to_owned(),
        ));
    }
    if entries.is_empty() {
        return Err(ParseError::Metadata {
            detail: "the `packages` array names no package with a manifest path".to_owned(),
        });
    }
    // Longest directory first, so that a package nested inside another wins over its parent. Ties
    // are broken by name and version so the order does not depend on the input's.
    entries.sort_by(|a, b| {
        b.0.len()
            .cmp(&a.0.len())
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    Ok(PackageIndex {
        workspace_root: value
            .get("workspace_root")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        packages: entries,
    })
}

fn normalise(path: &str) -> String {
    path.replace('\\', "/")
}

fn is_absolute(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    // A Windows path: a drive letter, a colon, and a separator.
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(letter), Some(':'), Some('/')) if letter.is_ascii_alphabetic()
    )
}

fn under(path: &str, directory: &str) -> bool {
    let directory = directory.trim_end_matches('/');
    match path.strip_prefix(directory) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

fn parent_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(at) => path.get(..at).unwrap_or(""),
        None => "",
    }
}
