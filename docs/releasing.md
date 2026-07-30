# Releasing to crates.io

This workspace publishes two crates, and the order matters. Read the whole page before running
anything: the first command that succeeds cannot be undone.

## What cannot be undone

**A published version cannot be deleted, only yanked.** Yanking tells Cargo not to select the
version for new dependency resolution. The version stays on crates.io, its files stay downloadable,
its documentation stays up, and any `Cargo.lock` that already pins it keeps resolving. It exists to
stop *new* projects picking a version you have decided is broken, not to make one disappear:

    cargo yank --version 0.1.0 critpath-core     # and --undo to reverse it

**A whole crate can be deleted, under conditions, and only from the website.** There is no Cargo
subcommand. The registry allows an owner to delete a crate when either:

- it was first published **less than 72 hours** ago; or
- it has a **single owner**, has been downloaded fewer than **1000 times for each month** it has
  existed, and **no crate on crates.io depends on it**.

A team cannot delete; only a user owner can. The name becomes available again **24 hours** after
deletion.

That 72-hour window is the real escape hatch for a first release: if `critpath-core 0.1.0` goes out
with something wrong in the tarball, it can be removed and the version number reused. After the
window closes, or once the other crate depends on it, the only remedy is to yank and publish a new
version.

Outside those conditions the **crate name** is permanent. Nobody else can take it, and a crate
cannot be renamed — a new name is a new crate and the old one keeps existing.

So treat the version number, the contents of the tarball, and the metadata on the crate page as
one-way. That is why `--dry-run` exists and why it should be run every time.

## Before you start

Confirm the working tree is committed and the tag exists, because the published tarball records the
commit it came from in `.cargo_vcs_info.json`, and a dirty tree either refuses to package or records
a commit that does not describe what you shipped.

    git status --short          # must be empty
    git describe --tags         # must be the version you are about to publish

Confirm both crates carry the version you intend, and that it matches the tag:

    cargo metadata --no-deps --format-version 1 | python3 -c \
      'import json,sys; [print(p["name"], p["version"]) for p in json.load(sys.stdin)["packages"]]'

Run the full gate. A published version that does not compile is a permanent embarrassment:

    cargo test --workspace --all-targets --all-features
    cargo test --workspace --doc
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo fmt --all --check
    cargo +1.74.0 build --workspace          # the declared minimum
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

## Getting a token

1. Sign in to <https://crates.io> with the GitHub account that should own the crates. The crate
   owner is whoever publishes first, so this decision is as permanent as the name.
2. Under Account Settings, API Tokens, create a token. Give it the narrowest scope that works:
   `publish-new` is needed for the very first publish of each crate, `publish-update` for every
   version after that. Do not create a token with more scopes than the release needs.
3. Set an expiry. A token without one is a credential that leaks quietly.

Then, once:

    cargo login

It prompts for the token and writes it to `$CARGO_HOME/credentials.toml` (`~/.cargo` by default) with
owner-only permissions. Do not pass the token on the command line — it lands in your shell history.
For CI, prefer `CARGO_REGISTRY_TOKEN` in the environment over a stored file.

## The ordering constraint

`cargo-critpath` depends on `critpath-core`. That dependency is declared with both a path and a
version:

    critpath-core = { version = "0.1.0", path = "crates/critpath-core" }

The path is what the workspace builds against; the version is what the published crate records. The
registry will not accept a package whose dependency cannot be resolved from the index, so
`critpath-core` must reach the index before `cargo-critpath` is uploaded.

Publishing them one at a time makes that ordering the operator's problem, and worse, it makes the
first upload — the irreversible one — happen while the second crate is still unverified. Publishing
them one at a time also means this fails, which is correct but unhelpful as a pre-flight:

    $ cargo publish --dry-run -p cargo-critpath
    error: failed to prepare local package for uploading
    Caused by:
      no matching package named `critpath-core` found
      location searched: crates.io index

Since Cargo 1.90, `--workspace` handles both crates in one command and works out the order itself,
resolving the inter-crate dependency through a temporary local registry rather than the real index.
That verifies *both* tarballs before anything is uploaded, which is what makes the first upload safe:

    cargo publish --dry-run --workspace

**`cargo publish --workspace` is not atomic.** A server-side error partway through can leave the
workspace partially published — the first crate live and the second not. That is recoverable, since
the missing crate can simply be published again, but do not assume a failure means nothing happened.

On Cargo older than 1.90, fall back to publishing in dependency order by hand: `critpath-core`
first, wait for the index, then `cargo-critpath`.

## Publishing

Dry-run the whole workspace first. This packages both crates, extracts each tarball, and compiles it
in isolation, which is the only check that catches a file the tests need but the package omits:

    cargo publish --dry-run --workspace

Read both file lists it prints. Anything unexpected in there is public forever. Expect 21 files for
`critpath-core` and 40 for `cargo-critpath`; a number that has dropped means something the tests read
is no longer being shipped.

Then release:

    cargo publish --workspace

Confirm both are live, and that the versions resolve from the index rather than from your checkout:

    cargo search critpath-core
    cargo search cargo-critpath

Finally, push the tag:

    git push origin v0.1.0

## Releasing a later version

Both crates share one version number through `[workspace.package]`, and the dependency between them
pins it. To release `0.2.0`:

1. Change `version` in the root `Cargo.toml` under `[workspace.package]`.
2. Change the version requirement on the path dependency in the same file, in
   `[workspace.dependencies]`. Forgetting this publishes a `cargo-critpath` that depends on the *old*
   `critpath-core`, which resolves and compiles, so nothing will tell you.
3. Regenerate both golden files. They embed the crate version, so the suite fails until they are
   rebuilt — loudly, but it is easy to mistake for a real regression:

       cargo run -q -p cargo-critpath -- critpath \
         --log crates/cargo-critpath/tests/fixtures/workspace/verbose.log \
         --timings crates/cargo-critpath/tests/fixtures/workspace/cargo-timing.html \
         --metadata crates/cargo-critpath/tests/fixtures/workspace/cargo-metadata.json \
         > crates/cargo-critpath/tests/golden/report.txt

   and the same with `--format json --include-units` into `tests/golden/analysis.json`. The exact
   commands are in the `REGENERATE` constants in `tests/report.rs` and `tests/json.rs`. The version
   literal in `docs/json-schema.md`'s example needs the same treatment.
4. Run `cargo build` so `Cargo.lock` is updated, and commit everything together.
5. Tag it, annotated, with a message that says what changed and what is still unresolved.
6. Repeat the gate, then `cargo publish --dry-run --workspace`, then `cargo publish --workspace`.

Semantic versioning applies to the library API of `critpath-core` and to the command line and JSON
schema of `cargo-critpath`. The JSON document carries its own `schema_version` independently — see
[json-schema.md](json-schema.md) — and a patch release must never change it. Decisions D4 and D18 in
[decisions.md](decisions.md) record what is promised at 0.x and what is not.

If a release turns out to be wrong, yank it, fix the problem, and publish the next patch version.
Do not attempt to reuse the number: crates.io will refuse it, and that refusal is the feature.
