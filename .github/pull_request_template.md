## What and why

<!-- Link the issue. Explain the change in your own words. -->

## Automated testing

<!-- What tests cover this change, and what did you run? -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features` and `cargo deny check` pass
- [ ] Every doc re-checked against this change (README, CONTRIBUTING, `docs/ARCHITECTURE.md`, the book), not only the page I expected to change
- [ ] Affected book pages updated, saying plainly what does not work yet
- [ ] Screenshots regenerated if the interface changed (`UPDATE_SNAPSHOTS=1 HELIA_GPU_TESTS=software cargo test -p helia-gui snapshots`) and the page text beside them checked
- [ ] `CHANGELOG.md` updated under Unreleased
- [ ] `cargo xtask reuse-check` passes (new files have the SPDX header, or a `REUSE.toml` annotation)
- [ ] `cargo xtask docs` run and the result committed (`cargo xtask docs --check` passes)
- [ ] `zensical build --strict --clean` passes with no link errors
