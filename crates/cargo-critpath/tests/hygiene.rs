//! The fixtures are real build output, and real build output is full of the machine it came from.
//!
//! This test is what makes the sanitisation described in `tests/fixtures/README.md` a property of
//! the repository rather than a step someone remembered to run.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;

/// Fragments that would mean a real machine's filesystem leaked into a committed file.
const FORBIDDEN: [&str; 4] = ["/Users/", "/home/", "C:\\Users\\", "/root/"];

fn check(directory: &Path, findings: &mut Vec<String>) {
    let entries = fs::read_dir(directory).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            check(&path, findings);
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            for fragment in FORBIDDEN {
                if line.contains(fragment) {
                    findings.push(format!(
                        "{}:{} contains {fragment:?}",
                        path.display(),
                        number.saturating_add(1)
                    ));
                }
            }
        }
    }
}

#[test]
fn no_fixture_carries_an_absolute_path_from_the_machine_that_produced_it() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut findings = Vec::new();
    check(&fixtures, &mut findings);
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}
