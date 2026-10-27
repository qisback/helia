// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia-manifest`: build, sign and verify update manifests (release pipeline tool).
//!
//! ```text
//! helia-manifest build  --app helia --channel stable --version 0.8.0 --sequence 42 \
//!     --base-url https://downloads.12hobbies.studio/helia/0.8.0/ --expires-days 60 \
//!     --asset linux-x86_64:appimage:dist/Helia-0.8.0-x86_64.AppImage --out manifest.json
//! helia-manifest sign   --manifest manifest.json --secret-key-file KEY   # writes manifest.json.minisig
//! helia-manifest verify --manifest manifest.json --key PUB [--key PUB2]
//! helia-manifest keygen-for-tests --out-dir DIR                          # throwaway keys, tests only
//! ```
//!
//! The secret key is read from the file named by `--secret-key-file` or, when that
//! is absent, by the environment variable `HELIA_UPDATE_SECRET_KEY_FILE`. It is
//! never printed and never taken from the command line itself.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use helia_update::manifest::{AssetDoc, Kind, ManifestDoc, ParseOptions, Platform, from_doc};
use helia_update::sign::KeyPair;
use helia_update::source::UpdateSource;
use helia_update::time::{format_utc, parse_utc};
use helia_update::verify::sha256_hex;
use helia_update::{Clock, SystemClock, verify_manifest};

#[derive(Parser)]
#[command(
    name = "helia-manifest",
    about = "Build, sign and verify update manifests"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Hash the asset files and write the manifest JSON.
    Build {
        #[arg(long)]
        app: String,
        #[arg(long, default_value = "stable")]
        channel: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        sequence: u64,
        /// Release date, `YYYY-MM-DDTHH:MM:SSZ`; default now.
        #[arg(long)]
        date: Option<String>,
        /// Expiry, `YYYY-MM-DDTHH:MM:SSZ`; or use --expires-days.
        #[arg(long)]
        expires: Option<String>,
        /// Days from the date until the manifest expires (default 60).
        #[arg(long)]
        expires_days: Option<u64>,
        #[arg(long)]
        min_supported: Option<String>,
        #[arg(long)]
        notes_url: Option<String>,
        /// URL prefix the asset file names are appended to (ends with `/`).
        #[arg(long)]
        base_url: String,
        /// `platform:kind:path`, repeatable.
        #[arg(long = "asset")]
        assets: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        /// Accept plain `http://127.0.0.1` addresses (the end-to-end test only).
        #[arg(long)]
        allow_loopback_http: bool,
    },
    /// Sign a manifest; writes `<manifest>.minisig` next to it.
    Sign {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        secret_key_file: Option<PathBuf>,
    },
    /// Verify a manifest and its `.minisig` against pinned keys and the policy.
    Verify {
        #[arg(long)]
        manifest: PathBuf,
        /// Signature file; default `<manifest>.minisig`.
        #[arg(long)]
        sig: Option<PathBuf>,
        /// Public key file or base64 line, repeatable (one or two).
        #[arg(long = "key")]
        keys: Vec<String>,
        /// Treat the manifest as valid at this time instead of now.
        #[arg(long)]
        at: Option<String>,
    },
    /// Write a throwaway key pair for tests. Never use these keys for a release.
    KeygenForTests {
        #[arg(long)]
        out_dir: PathBuf,
    },
}

fn read_text(p: &Path) -> Result<String, String> {
    fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))
}

#[allow(clippy::too_many_lines, clippy::similar_names)]
fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Build {
            app,
            channel,
            version,
            sequence,
            date,
            expires,
            expires_days,
            min_supported,
            notes_url,
            base_url,
            assets,
            out,
            allow_loopback_http,
        } => {
            let date = date.unwrap_or_else(|| format_utc(SystemClock.now_unix()));
            let date_s = parse_utc(&date).ok_or("--date is not YYYY-MM-DDTHH:MM:SSZ")?;
            let expires = match (expires, expires_days) {
                (Some(e), None) => e,
                (None, d) => format_utc(date_s + d.unwrap_or(60) * 86_400),
                _ => return Err("use --expires or --expires-days, not both".into()),
            };
            let mut docs = Vec::new();
            for spec in &assets {
                let mut it = spec.splitn(3, ':');
                let (Some(plat), Some(kind), Some(path)) = (it.next(), it.next(), it.next()) else {
                    return Err(format!("--asset {spec:?} is not platform:kind:path"));
                };
                if Platform::parse(plat).is_none() || Kind::parse(kind).is_none() {
                    return Err(format!("--asset {spec:?}: unknown platform or kind"));
                }
                let data = fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
                let name = Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("bad asset path")?;
                docs.push(AssetDoc {
                    platform: plat.into(),
                    kind: kind.into(),
                    url: format!("{base_url}{name}"),
                    sha256: sha256_hex(&data),
                    size: data.len() as u64,
                });
            }
            let doc = ManifestDoc {
                schema: 1,
                app,
                channel,
                version,
                date,
                sequence,
                expires,
                min_supported,
                notes_url,
                assets: docs,
            };
            from_doc(
                &doc,
                ParseOptions {
                    allow_loopback_http,
                },
            )
            .map_err(|e| e.to_string())?;
            let mut json = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
            json.push('\n');
            fs::write(&out, json).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
            println!("wrote {}", out.display());
        }
        Cmd::Sign {
            manifest,
            secret_key_file,
        } => {
            let key_path = secret_key_file
                .or_else(|| std::env::var_os("HELIA_UPDATE_SECRET_KEY_FILE").map(PathBuf::from))
                .ok_or("give --secret-key-file or set HELIA_UPDATE_SECRET_KEY_FILE")?;
            let kp =
                KeyPair::from_secret_key_file(&read_text(&key_path)?).map_err(|e| e.to_string())?;
            let data = fs::read(&manifest)
                .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
            let name = manifest
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("manifest.json");
            let sig = kp.sign(
                &data,
                &format!("timestamp:{}\tfile:{name}", SystemClock.now_unix()),
            );
            let mut out = manifest.clone().into_os_string();
            out.push(".minisig");
            fs::write(&out, sig).map_err(|e| format!("cannot write signature: {e}"))?;
            println!(
                "signed {} (public key {})",
                manifest.display(),
                kp.public_key()
            );
        }
        Cmd::Verify {
            manifest,
            sig,
            keys,
            at,
        } => {
            let data = fs::read(&manifest)
                .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
            let sig_path = sig.unwrap_or_else(|| {
                let mut s = manifest.clone().into_os_string();
                s.push(".minisig");
                s.into()
            });
            let keys: Vec<String> = keys
                .iter()
                .map(|k| {
                    if Path::new(k).is_file() {
                        read_text(Path::new(k))
                    } else {
                        Ok(k.clone())
                    }
                })
                .collect::<Result<_, _>>()?;
            let doc: ManifestDoc = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
            let now = match at {
                Some(a) => parse_utc(&a).ok_or("--at is not YYYY-MM-DDTHH:MM:SSZ")?,
                None => SystemClock.now_unix(),
            };
            let mut src = UpdateSource::new(
                &doc.app,
                &doc.channel,
                "https://invalid.example/manifest.json",
                &[],
            );
            src.pinned_keys = keys;
            let mut hosts: Vec<String> = doc
                .assets
                .iter()
                .filter_map(|a| helia_update::manifest::url_host(&a.url))
                .collect();
            hosts.sort();
            hosts.dedup();
            src.asset_hosts = hosts;
            let m = verify_manifest(&src, &data, &read_text(&sig_path)?, now, 0, false)
                .map_err(|e| e.to_string())?;
            println!(
                "ok: {} {} {} sequence {}",
                m.app, m.channel, m.version, m.sequence
            );
        }
        Cmd::KeygenForTests { out_dir } => {
            let kp = KeyPair::generate().map_err(|e| e.to_string())?;
            fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
            fs::write(out_dir.join("test.pub"), kp.public_key_file()).map_err(|e| e.to_string())?;
            let sk = out_dir.join("test.key");
            fs::write(&sk, kp.secret_key_file()).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sk, fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            }
            println!(
                "wrote throwaway test.pub and test.key in {}",
                out_dir.display()
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("helia-manifest: {e}");
            ExitCode::FAILURE
        }
    }
}
