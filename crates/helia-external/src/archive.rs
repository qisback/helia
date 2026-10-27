// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Unpack only the named files from a verified download.
//!
//! The archive has already passed its sha256 check, but it is still treated as
//! hostile: archive paths are only ever compared, never used to build an output
//! path (the output name is the manifest's `save_as`), only regular files are
//! read, and every file is capped in size.
//!
//! Formats: zip (the `zip` crate, MIT) and tar compressed with xz (`tar`, MIT or
//! Apache-2.0; `lzma-rs`, MIT). The xz stream is decompressed on a helper thread and
//! handed to the tar reader in chunks, so nothing large is written to disk twice, and
//! reading stops as soon as the wanted files are found.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::manifest::{ArchiveKind, Member, hex};

/// Largest single file kept from an archive, in bytes (1 GiB).
pub const MAX_MEMBER_BYTES: u64 = 1 << 30;

/// A file written to the item folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    /// Name it was saved as.
    pub save_as: String,
    /// Logical name, if the manifest gave one.
    pub provides: Option<String>,
    /// sha256 of what was written.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

fn archive_err(e: impl std::fmt::Display) -> Error {
    Error::Archive(e.to_string())
}

/// Does the archive path `entry` match the manifest's `find`? Whole components only.
fn matches(find: &str, entry: &str) -> bool {
    let entry = entry.strip_prefix("./").unwrap_or(entry);
    entry == find || entry.ends_with(&format!("/{find}"))
}

/// Copy `from` to `out_dir/<save_as>`, capped, hashing as it goes.
fn write_member(mut from: impl Read, out_dir: &Path, m: &Member) -> Result<Extracted> {
    let path = out_dir.join(&m.save_as);
    let mut out = BufWriter::new(File::create(&path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_MEMBER_BYTES {
            drop(out);
            let _ = std::fs::remove_file(&path);
            return Err(Error::Archive(format!(
                "{} is larger than the {MAX_MEMBER_BYTES} byte limit",
                m.save_as
            )));
        }
        h.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    drop(out);
    #[cfg(unix)]
    if m.executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(Extracted {
        save_as: m.save_as.clone(),
        provides: m.provides.clone(),
        sha256: hex(&h.finalize()),
        size,
    })
}

/// Unpack `members` of the archive at `archive` into `out_dir` (which must exist).
///
/// # Errors
/// [`Error::Archive`], [`Error::Member`] or [`Error::Io`].
pub fn extract(
    archive: &Path,
    kind: ArchiveKind,
    members: &[Member],
    out_dir: &Path,
) -> Result<Vec<Extracted>> {
    match kind {
        ArchiveKind::None => {
            let [m] = members else {
                return Err(Error::Archive("a plain file has exactly one name".into()));
            };
            Ok(vec![write_member(File::open(archive)?, out_dir, m)?])
        }
        ArchiveKind::Zip => extract_zip(archive, members, out_dir),
        ArchiveKind::TarXz => extract_tar_xz(archive, members, out_dir),
    }
}

fn extract_zip(archive: &Path, members: &[Member], out_dir: &Path) -> Result<Vec<Extracted>> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive)?)).map_err(archive_err)?;
    let mut done = Vec::new();
    for m in members {
        let mut index = None;
        for i in 0..z.len() {
            let e = z.by_index_raw(i).map_err(archive_err)?;
            let symlink = e
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000);
            if e.is_file() && !symlink && e.name().is_ok_and(|n| matches(&m.find, &n)) {
                index = Some(i);
                break;
            }
        }
        let i = index.ok_or_else(|| Error::Member(m.find.clone()))?;
        let e = z.by_index(i).map_err(archive_err)?;
        done.push(write_member(e, out_dir, m)?);
    }
    Ok(done)
}

/// Bytes arriving from the decompressor thread.
struct ChunkReader {
    rx: mpsc::Receiver<Vec<u8>>,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.cur.len() {
            match self.rx.recv() {
                Ok(c) => {
                    self.cur = c;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Sends what is written to the tar reader; fails once the reader has gone.
struct ChunkWriter(mpsc::SyncSender<Vec<u8>>);

impl Write for ChunkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "reader finished"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn extract_tar_xz(archive: &Path, members: &[Member], out_dir: &Path) -> Result<Vec<Extracted>> {
    let file = File::open(archive)?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(8);
    let decoder = thread::spawn(move || -> std::result::Result<(), String> {
        let mut input = BufReader::new(file);
        let mut out = BufWriter::with_capacity(256 * 1024, ChunkWriter(tx));
        lzma_rs::xz_decompress(&mut input, &mut out).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())
    });

    let result = (|| {
        let mut tar = tar::Archive::new(ChunkReader {
            rx,
            cur: Vec::new(),
            pos: 0,
        });
        let mut todo: Vec<&Member> = members.iter().collect();
        let mut done = Vec::new();
        for entry in tar.entries().map_err(archive_err)? {
            let entry = entry.map_err(archive_err)?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry
                .path()
                .map_err(archive_err)?
                .to_string_lossy()
                .into_owned();
            if let Some(at) = todo.iter().position(|m| matches(&m.find, &path)) {
                let m = todo.remove(at);
                done.push(write_member(entry, out_dir, m)?);
                if todo.is_empty() {
                    break;
                }
            }
        }
        if let Some(m) = todo.first() {
            return Err(Error::Member(m.find.clone()));
        }
        Ok(done)
    })();

    // The tar reader is gone, so a decoder still running fails to send and stops.
    let joined = decoder.join();
    match (&result, joined) {
        (Err(_), Ok(Err(why))) if !why.contains("reader finished") => {
            return Err(Error::Archive(why));
        }
        (_, Err(_)) => {
            return Err(Error::Archive(
                "the unpacking thread stopped unexpectedly".into(),
            ));
        }
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn member(find: &str, save_as: &str) -> Member {
        Member {
            find: find.into(),
            save_as: save_as.into(),
            provides: Some(save_as.into()),
            executable: true,
        }
    }

    pub(crate) fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    pub(crate) fn make_tar_xz(path: &Path, files: &[(&str, &[u8])]) {
        let mut tarball = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tarball);
            for (name, data) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, *data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut Cursor::new(tarball), &mut xz).unwrap();
        std::fs::write(path, xz).unwrap();
    }

    #[test]
    fn components_match_whole_names_only() {
        assert!(matches("bin/ffmpeg", "ffmpeg-n9/bin/ffmpeg"));
        assert!(matches("bin/ffmpeg", "bin/ffmpeg"));
        assert!(matches("ffmpeg", "./ffmpeg"));
        assert!(!matches("bin/ffmpeg", "ffmpeg-n9/xbin/ffmpeg"));
        assert!(!matches("ffmpeg", "ffmpeg.exe"));
    }

    #[test]
    fn zip_members_are_extracted_by_name_not_by_path() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.zip");
        make_zip(
            &a,
            &[
                ("pkg/doc/readme.txt", b"hi"),
                ("pkg/bin/ffmpeg.exe", b"MZ-ffmpeg"),
                ("pkg/bin/ffprobe.exe", b"MZ-ffprobe"),
                ("../../evil", b"x"),
            ],
        );
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let got = extract(
            &a,
            ArchiveKind::Zip,
            &[
                member("bin/ffmpeg.exe", "ffmpeg.exe"),
                member("bin/ffprobe.exe", "ffprobe.exe"),
            ],
            &out,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(std::fs::read(out.join("ffmpeg.exe")).unwrap(), b"MZ-ffmpeg");
        assert_eq!(
            got[0].sha256,
            crate::manifest::hex(&Sha256::digest(b"MZ-ffmpeg"))
        );
        assert!(!d.path().join("evil").exists());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    }

    #[test]
    fn a_missing_member_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.zip");
        make_zip(&a, &[("pkg/bin/other", b"x")]);
        let r = extract(
            &a,
            ArchiveKind::Zip,
            &[member("bin/ffmpeg", "ffmpeg")],
            d.path(),
        );
        assert!(matches!(r, Err(Error::Member(_))));
    }

    #[test]
    fn tar_xz_members_are_extracted_and_reading_stops_early() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.tar.xz");
        let big = vec![7u8; 3_000_000];
        make_tar_xz(
            &a,
            &[
                ("ffmpeg-n9/bin/ffmpeg", b"ELF-ffmpeg"),
                ("ffmpeg-n9/bin/ffprobe", b"ELF-ffprobe"),
                ("ffmpeg-n9/doc/huge.html", &big),
            ],
        );
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let got = extract(
            &a,
            ArchiveKind::TarXz,
            &[
                member("bin/ffprobe", "ffprobe"),
                member("bin/ffmpeg", "ffmpeg"),
            ],
            &out,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(std::fs::read(out.join("ffprobe")).unwrap(), b"ELF-ffprobe");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(out.join("ffmpeg"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn tar_xz_missing_member_and_corrupt_stream_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.tar.xz");
        make_tar_xz(&a, &[("x/bin/other", b"1")]);
        let r = extract(
            &a,
            ArchiveKind::TarXz,
            &[member("bin/ffmpeg", "ffmpeg")],
            d.path(),
        );
        assert!(matches!(r, Err(Error::Member(_))), "{r:?}");

        let bad = d.path().join("bad.tar.xz");
        std::fs::write(&bad, b"this is not an xz file at all").unwrap();
        let r = extract(
            &bad,
            ArchiveKind::TarXz,
            &[member("bin/ffmpeg", "ffmpeg")],
            d.path(),
        );
        assert!(matches!(r, Err(Error::Archive(_))), "{r:?}");
    }

    #[test]
    fn a_plain_file_is_copied_under_the_manifest_name() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("blob");
        std::fs::write(&a, b"data").unwrap();
        let out = d.path().join("o");
        std::fs::create_dir(&out).unwrap();
        let got = extract(&a, ArchiveKind::None, &[member("", "pack.bin")], &out).unwrap();
        assert_eq!(got[0].size, 4);
        assert_eq!(std::fs::read(out.join("pack.bin")).unwrap(), b"data");
    }
}
