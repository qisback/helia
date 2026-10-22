# Contributing to Helia

**Outside contributions are not being accepted yet.** Until a contributor agreement
(CLA or DCO) is decided, pull requests from other people will be closed unread. Issues are
welcome: bug reports, questions and ideas. This page describes how the project is
worked on today.

Helia is GPL-3.0-or-later.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
cargo xtask docs --check
cargo xtask reuse-check
zensical build --strict --clean
```

CI runs far more than this (six targets, software-GPU parity, Miri, fuzzing, real-data tests,
coverage, golden-image regressions), so a green local run is the minimum.

## Documentation is part of the change

Docs are produced with every change and must stay true. The book lives in
`docs/book` ([Zensical](https://zensical.org/), configured by `zensical.toml`); `docs/ARCHITECTURE.md` and `docs/research/` are included
in it. Every pull request:

1. **Re-checks all docs**, not only the page you expect to be affected. Read the
   README, `CONTRIBUTING.md`, `docs/ARCHITECTURE.md` and the book for anything
   your change makes untrue: a flag, a default, a keyboard shortcut, what runs on
   the GPU, a "not yet implemented" note.
2. **Updates the affected pages** in the same pull request. A new option, route
   or screen needs its explanation, not just its code. Say plainly what does not
   work yet, and quote performance figures only if measured, with the hardware
   and the commit.
3. **Regenerates screenshots** when the interface changes:
   `UPDATE_SNAPSHOTS=1 HELIA_GPU_TESTS=software cargo test -p helia-gui snapshots`.
   Look at the changed images, then update the page text beside them.
4. **Updates `CHANGELOG.md`** under *Unreleased*.
5. **Runs `cargo xtask docs`** and commits the result. It regenerates the CLI
   reference (from the clap definition in `crates/helia-cli/src/lib.rs`), the API
   reference and `openapi.json` (from the OpenAPI document `helia-server`
   builds), all under `docs/book/src/reference/`. CI runs
   `cargo xtask docs --check` and fails when the committed files differ. Never
   edit those three files by hand: change the doc comments or annotations they
   come from.
6. **Builds the book** with `zensical build --strict --clean`. Strict mode fails on
   a broken link, image or include. Install it once with
   `python3 -m venv .venv && .venv/bin/pip install -r docs/requirements.txt`. A new page
   goes into the `nav` list in `zensical.toml` (`cargo xtask coverage` checks this).

The pull request template has these as checkboxes. Documentation is written in
British English, with plain, friendly prose for users and precise prose for
developers.

## Copyright and licence headers

Helia is GPL-3.0-or-later with the additional terms in
[ADDITIONAL-TERMS.md](ADDITIONAL-TERMS.md) (GPL section 7(b) and 7(c): the
author attribution and the dedication are preserved, and modified versions are
marked as changed). The terms for outside contributions (copyright and any
contributor agreement) are not settled yet, which is why they are not accepted. The project follows the
[REUSE specification](https://reuse.software/spec/):

- Every source file (Rust, WGSL, Python, shell, PowerShell, workflow YAML) starts
  with three comment lines, in that language's comment syntax (after a `#!` line
  in scripts):

  ```
  // SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
  // SPDX-License-Identifier: GPL-3.0-or-later
  // See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
  ```

- If you substantially change a file, add your own line above or below the
  existing one, for example `SPDX-FileCopyrightText: 2027 Your Name`. Do not
  remove anyone else's line.
- Documentation, images and configuration that cannot carry a header are covered
  by an annotation in `REUSE.toml`. Third-party files keep their own copyright
  and licence there.
- `cargo xtask reuse-check` fails when a file has neither, and CI runs it.

## Rules of the codebase

- Every GPU kernel has a CPU reference implementation, and a test that the two
  agree within a stated tolerance. The CPU path is the specification.
- Image maths is tested on synthetic data with known answers (a known shift,
  a known blur), not only on "looks right" samples.
- No `unsafe` outside crates that explicitly allow it, and those run under Miri.
- New dependencies must pass `cargo deny` (GPL-compatible licences only).
- British English in user-facing text and documentation.

## Reporting bugs

The easiest way is **Feedback** in the app (Ctrl+Shift+F) or `helia feedback`: they
file it in the issue tracker for you, with a screenshot you can draw on and, if you tick it,
your version and system details. Otherwise include the OS, GPU and driver,
`helia --version`, and if possible a short capture (a few hundred frames) that
shows the problem.
