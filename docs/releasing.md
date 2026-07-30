# Releasing to crates.io

This workspace publishes two crates, and the order matters. Read the whole page before running
anything: the first command that succeeds cannot be undone.

## What cannot be undone

Publishing a version is permanent. A published version can be **yanked**, which is not deletion:

- **Yanking** tells Cargo not to select the version for new dependency resolution. The version stays
  on crates.io, its files stay downloadable, its documentation stays up, and any `Cargo.lock` that
  already pins it keeps resolving. It exists to stop *new* projects picking a version you have
  decided is broken. `cargo yank --version 0.1.0 critpath-core`, and `--undo` to reverse it.
- **Deletion** is not available. There is no command, and the registry will not do it on request
  except for genuine legal or security emergencies. Assume every byte you upload is public forever.

The **crate name** is also permanent. Publishing `critpath-core` reserves that name for this
account; nobody else can ever take it, and you cannot rename the crate — a new name is a new crate
and the old one keeps existing.

So the version number, the contents of the tarball, and the metadata on the crates.io page are all
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

The path is what the workspace builds against; the version is what the published crate records.
crates.io does not accept a package whose dependency cannot be resolved from the registry, so
**`critpath-core` must be published, and its version must exist on the index, before
`cargo-critpath` can be packaged at all** — not merely before it is published. Attempting the
dependent crate first fails with:

    error: failed to prepare local package for uploading
    Caused by:
      no matching package named `critpath-core` found
      location searched: crates.io index

This also means `cargo publish --dry-run -p cargo-critpath` cannot succeed before the first crate is
released. That is expected, and it is not a reason to skip the dry run on the other crate.

## Publishing

Dry-run first. This packages the crate, extracts the tarball, and compiles it in isolation, which is
the only check that catches a file the tests need but the package does not contain:

    cargo publish --dry-run -p critpath-core

Read the file list it prints. Anything unexpected in there is public forever.

Then, in order:

    cargo publish -p critpath-core

Wait for the index to update — usually seconds, occasionally a minute. Confirm the version is
resolvable before continuing, because the next command reads the index:

    cargo search critpath-core

Now the second crate can be verified and published:

    cargo publish --dry-run -p cargo-critpath
    cargo publish -p cargo-critpath

Finally, push the tag if it is not already pushed:

    git push origin v0.1.0

## Releasing a later version

Both crates share one version number through `[workspace.package]`, and the dependency between them
pins it. To release `0.2.0`:

1. Change `version` in the root `Cargo.toml` under `[workspace.package]`.
2. Change the version requirement on the path dependency in the same file, in
   `[workspace.dependencies]`. Forgetting this publishes a `cargo-critpath` that depends on the *old*
   `critpath-core`, which resolves and compiles, so nothing will tell you.
3. Run `cargo build` so `Cargo.lock` is updated, and commit both.
4. Tag it, annotated, with a message that says what changed and what is still unresolved.
5. Repeat the gate and the two publishes in the same order.

Semantic versioning applies to the library API of `critpath-core` and to the command line and JSON
schema of `cargo-critpath`. The JSON document carries its own `schema_version` independently — see
[json-schema.md](json-schema.md) — and a patch release must never change it. Decisions D4 and D18 in
[decisions.md](decisions.md) record what is promised at 0.x and what is not.

If a release turns out to be wrong, yank it, fix the problem, and publish the next patch version.
Do not attempt to reuse the number: crates.io will refuse it, and that refusal is the feature.
