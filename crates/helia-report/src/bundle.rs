// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! What a report is made of: files, and the sections that present them.

use serde::Serialize;

/// What a report file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// A figure.
    Png,
    /// The numbers behind a figure, for a spreadsheet or Python.
    Csv,
    /// The same numbers with their settings; `helia plot` redraws from these.
    Json,
    /// The self-contained report.
    Html,
    /// Plain text (a FITS header summary).
    Text,
}

impl FileKind {
    /// The `Content-Type` of the file.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Csv => "text/csv; charset=utf-8",
            Self::Json => "application/json",
            Self::Html => "text/html; charset=utf-8",
            Self::Text => "text/plain; charset=utf-8",
        }
    }

    /// The kind a file name's extension says.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name.rsplit('.').next().unwrap_or("") {
            "png" => Self::Png,
            "csv" => Self::Csv,
            "json" => Self::Json,
            "html" => Self::Html,
            _ => Self::Text,
        }
    }
}

/// One file of a report.
#[derive(Clone, Debug)]
pub struct ReportFile {
    /// File name, no directory.
    pub name: String,
    /// What it is.
    pub kind: FileKind,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// A figure in a section: the PNG, how to read it, where its numbers are.
#[derive(Clone, Debug)]
pub struct Figure {
    /// Name of the PNG file.
    pub png: String,
    /// Heading.
    pub title: String,
    /// How to read it, a few sentences.
    pub caption: String,
    /// Names of the data files behind it.
    pub data: Vec<String>,
}

/// A small table in a section.
#[derive(Clone, Debug)]
pub struct Table {
    /// Heading.
    pub title: String,
    /// Column names.
    pub header: Vec<String>,
    /// Rows of cells.
    pub rows: Vec<Vec<String>>,
}

/// A group of figures with an introduction.
#[derive(Clone, Debug)]
pub struct Section {
    /// Anchor name, lower case.
    pub id: String,
    /// Heading.
    pub title: String,
    /// What the section is for.
    pub intro: String,
    /// Figures, in order.
    pub figures: Vec<Figure>,
    /// Tables, after the figures.
    pub tables: Vec<Table>,
    /// Things that were not drawn, and why.
    pub notes: Vec<String>,
}

/// A finished report in memory.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    /// Every file, including the HTML.
    pub files: Vec<ReportFile>,
    /// The sections, in order.
    pub sections: Vec<Section>,
}

impl Bundle {
    /// The file called `name`.
    #[must_use]
    pub fn file(&self, name: &str) -> Option<&ReportFile> {
        self.files.iter().find(|f| f.name == name)
    }

    /// Add a file, replacing one of the same name.
    pub fn add(&mut self, name: &str, bytes: Vec<u8>) {
        self.files.retain(|f| f.name != name);
        self.files.push(ReportFile {
            name: name.to_string(),
            kind: FileKind::from_name(name),
            bytes,
        });
    }

    /// The figures as (figure, PNG bytes), for the app.
    #[must_use]
    pub fn figures(&self) -> Vec<(&Figure, &[u8])> {
        self.sections
            .iter()
            .flat_map(|s| &s.figures)
            .filter_map(|f| self.file(&f.png).map(|p| (f, p.bytes.as_slice())))
            .collect()
    }

    /// Write every file into `dir`, creating it.
    pub fn write(&self, dir: &std::path::Path) -> crate::Result<()> {
        std::fs::create_dir_all(dir)?;
        for f in &self.files {
            std::fs::write(dir.join(&f.name), &f.bytes)?;
        }
        Ok(())
    }
}
