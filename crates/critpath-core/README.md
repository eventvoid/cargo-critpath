# critpath-core

Schedule simulation and counterfactual analysis over a weighted build DAG, with no knowledge of
any particular build system.

This crate is the analysis engine behind `cargo-critpath`. It has no dependencies and performs no
I/O. See the repository's `docs/model.md` for the schedule model, its invariants, and the errors it
is known to make.
