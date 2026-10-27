// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Documentation coverage check.
//!
//! Three rules. The first two are about prose written by hand (the generated reference
//! cannot go stale, so it does not count):
//!
//! 1. every subcommand and long flag in the clap tree is mentioned in the
//!    user guide, the step-by-step walkthrough or the recipes;
//! 2. the table in `reference/documentation-coverage.md` links only pages
//!    that exist, has a known status in every row, and does not call a row
//!    Done while a cell still says Partial or Missing;
//! 3. every page is in the `zensical.toml` navigation or deliberately excluded.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Command, CommandFactory};
use helia_cli::Cli;

/// Hand-written chapters whose text counts as documentation of a flag.
const GUIDE_DIRS: [&str; 3] = ["user-guide", "walkthrough", "recipes"];

/// The coverage page, relative to the book sources.
const COVERAGE_PAGE: &str = "reference/documentation-coverage.md";

/// Every `.md` file under `dir`, recursively.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            markdown_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Subcommand paths (`stack`, ...) and long flags (`--keep`, ...) of a command tree.
fn walk(cmd: &Command, prefix: &str, subs: &mut Vec<String>, flags: &mut Vec<String>) {
    for a in cmd.get_arguments() {
        if let Some(l) = a.get_long().filter(|l| !matches!(*l, "help" | "version")) {
            flags.push(format!("--{l}"));
        }
    }
    for s in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
        // A nested command is written with its parent: `plugins enable`, not `enable`.
        let path = format!("{prefix}{}", s.get_name());
        subs.push(path.clone());
        walk(s, &format!("{path} "), subs, flags);
    }
}

/// True when `text` contains `needle` not followed by a character that would
/// make it a longer flag or word (`--keep` must not match `--keep-frames`).
fn mentions(text: &str, needle: &str) -> bool {
    text.match_indices(needle).any(|(i, _)| {
        text[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    })
}

fn check_flags(src: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for d in GUIDE_DIRS {
        markdown_files(&src.join(d), &mut files)?;
    }
    let mut text = String::new();
    for f in &files {
        text.push_str(&std::fs::read_to_string(f)?);
        text.push('\n');
    }
    let (mut subs, mut flags) = (Vec::new(), Vec::new());
    walk(&Cli::command(), "", &mut subs, &mut flags);
    subs.sort();
    subs.dedup();
    flags.sort();
    flags.dedup();
    let mut problems = Vec::new();
    for s in subs {
        if !mentions(&text, &format!("helia {s}")) {
            problems.push(format!(
                "subcommand `helia {s}` is not mentioned in the user guide, walkthrough or recipes"
            ));
        }
    }
    for f in flags {
        if !mentions(&text, &f) {
            problems.push(format!(
                "flag `{f}` is not mentioned in the user guide, walkthrough or recipes"
            ));
        }
    }
    Ok(problems)
}

/// Link targets of one markdown line: the part inside `](...)`.
fn link_targets(line: &str) -> Vec<&str> {
    line.split("](")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .collect()
}

fn check_table(src: &Path) -> Result<Vec<String>> {
    let page = src.join(COVERAGE_PAGE);
    let text = std::fs::read_to_string(&page)
        .with_context(|| format!("cannot read {}", page.display()))?;
    let base = page.parent().unwrap_or(src);
    let mut problems = Vec::new();
    let mut rows = 0;
    for line in text.lines().filter(|l| l.starts_with("| ")) {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        if cells.first() == Some(&"Feature") {
            continue;
        }
        rows += 1;
        let feature = cells.first().copied().unwrap_or_default();
        if cells.len() != 6 {
            problems.push(format!("row `{feature}` has {} cells, not 6", cells.len()));
            continue;
        }
        let status = cells[4];
        if !matches!(status, "Done" | "Partial" | "Missing") {
            problems.push(format!(
                "row `{feature}`: status `{status}` is not Done, Partial or Missing"
            ));
        }
        if status == "Done"
            && cells[1..4]
                .iter()
                .any(|c| c.starts_with("Partial") || c.starts_with("Missing"))
        {
            problems.push(format!("row `{feature}` is Done but a column is not"));
        }
        if status != "Done"
            && cells[1..4]
                .iter()
                .all(|c| c.starts_with("Done") || *c == "n/a")
        {
            problems.push(format!(
                "row `{feature}` is {status} but every column is Done"
            ));
        }
        for t in link_targets(line) {
            if t.starts_with("http") || t.starts_with('#') {
                continue;
            }
            let file = t.split('#').next().unwrap_or(t);
            if !base.join(file).is_file() {
                problems.push(format!("row `{feature}` links `{t}`, which does not exist"));
            }
        }
    }
    if rows == 0 {
        problems.push("the coverage table has no rows".to_owned());
    }
    Ok(problems)
}

/// Page paths in a Zensical `nav` value (external links are skipped).
fn nav_pages(v: &toml::Value, out: &mut Vec<String>) {
    match v {
        toml::Value::String(s) if Path::new(s).extension().is_some_and(|e| e == "md") => {
            out.push(s.clone());
        }
        toml::Value::Array(a) => a.iter().for_each(|x| nav_pages(x, out)),
        toml::Value::Table(t) => t.values().for_each(|x| nav_pages(x, out)),
        _ => {}
    }
}

/// Every page under the docs directory is in `nav` or `exclude_docs` of
/// `zensical.toml`, and every `nav` page exists. Zensical itself reports
/// broken links, images and snippets when the site is built with `--strict`.
fn check_nav(root: &Path, src: &Path) -> Result<Vec<String>> {
    let cfg_path = root.join("zensical.toml");
    let text = std::fs::read_to_string(&cfg_path)
        .with_context(|| format!("cannot read {}", cfg_path.display()))?;
    let cfg: toml::Value = toml::from_str(&text).context("zensical.toml is not valid TOML")?;
    let project = cfg
        .get("project")
        .context("zensical.toml has no [project]")?;
    let mut nav = Vec::new();
    if let Some(n) = project.get("nav") {
        nav_pages(n, &mut nav);
    }
    let excluded: Vec<&str> = project
        .get("exclude_docs")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut problems = Vec::new();
    for page in &nav {
        if !src.join(page).is_file() {
            problems.push(format!("nav lists `{page}`, which does not exist"));
        }
    }
    let mut files = Vec::new();
    markdown_files(src, &mut files)?;
    for f in files {
        let rel = f
            .strip_prefix(src)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        if !nav.contains(&rel) && !excluded.contains(&rel.as_str()) {
            problems.push(format!(
                "`{rel}` is in neither `nav` nor `exclude_docs` in zensical.toml"
            ));
        }
    }
    Ok(problems)
}

/// Run all checks against the book sources under `root`.
pub fn check(root: &Path) -> Result<()> {
    let src = root.join("docs/book/src");
    let mut problems = check_flags(&src)?;
    problems.extend(check_table(&src)?);
    problems.extend(check_nav(root, &src)?);
    if problems.is_empty() {
        println!("documentation coverage checks pass");
        return Ok(());
    }
    bail!(
        "documentation coverage problems:\n  {}\nfix the guide or {COVERAGE_PAGE}",
        problems.join("\n  ")
    )
}
