// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `helia`: command-line lucky-imaging stacker.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser as _};
use helia_cli::plugins::PluginRun;
use helia_cli::{
    CalibrateArgs, ChannelArg, Cli, ColourChoice, Command, DebayerArg, EditCommandArgs, MetricArg,
    OnOff, OpenArgs, RobustArg, ServeArgs, StackArgs,
};
use helia_core::align::multipoint::MultipointConfig;
use helia_core::align::refine::SmoothConfig;
use helia_core::autoap::AutoAp;
use helia_core::debayer::{Channel, Debayer, Method};
use helia_core::edit::store;
use helia_core::io::ImageFormat;
use helia_core::io::avi::AviError;
use helia_core::io::capture::{CaptureReport, open_reported_with};
use helia_core::io::ffmpeg::{self, VideoOptions};
use helia_core::io::solarnet::{CaptureInfo, FitsMeta, Site};
use helia_core::io::source::FrameSource;
use helia_core::mask::MaskConfig;
use helia_core::output::{OutputPlan, Prepared, finish, prepare, write_loaded};
use helia_core::pipeline::{ApRefine, Pipeline, StackConfig, Stage};
use helia_core::recommend::{Recommendation, capture_texts, recommend};
use helia_core::stack::{RobustKind, StackMode};
use helia_core::truecolour::{TrueColour, WhiteBalance};
use helia_core::{ColourParams, Keep, Kernel, Metric, Preset, filter, quality};
use helia_gpu::select_backend;
use helia_plugin::StepStage;
use helia_update::backend::UpdateBackend;
use indicatif::{ProgressBar, ProgressStyle};

fn main() {
    if let Err(e) = run() {
        helia_core::applog::error(&format!("{e:#}"));
        eprintln!("helia: {e:#}");
        std::process::exit(1);
    }
}

/// Parse the command line. A backend's own command (an official store build adds one) is run
/// here and `None` is returned; everything else gives the parsed [`Cli`].
fn parse_cli(long_version: &'static str) -> Option<Cli> {
    let hook = helia_update::backend::installed().and_then(UpdateBackend::cli);
    let mut command = Cli::command().long_version(long_version);
    if let Some(h) = hook {
        // The backend's command takes everything after its name and parses it itself.
        command = command.subcommand(
            clap::Command::new(h.name)
                .about(h.about)
                .disable_help_flag(true)
                .disable_version_flag(true)
                .arg(
                    clap::Arg::new("args")
                        .num_args(0..)
                        .allow_hyphen_values(true)
                        .trailing_var_arg(true)
                        .value_parser(clap::value_parser!(std::ffi::OsString)),
                ),
        );
    }
    let matches = command.get_matches();
    if let (Some(h), Some((name, sub))) = (hook, matches.subcommand())
        && name == h.name
    {
        let args: Vec<std::ffi::OsString> = sub
            .get_many::<std::ffi::OsString>("args")
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        exit_unless_ok((h.run)(&args));
        return None;
    }
    Some(Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit()))
}

fn run() -> Result<()> {
    // An official store build plugs in a backend at start-up; a source build has none.
    #[cfg(feature = "store")]
    store_backend::install();
    // `-V` stays one line; `--version` adds the build record and the copyright,
    // licence and warranty notices. clap prints "helia " itself, so drop ours.
    static LONG_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let long = LONG_VERSION.get_or_init(|| {
        let text = helia_core::about::long_version(
            "helia",
            env!("CARGO_PKG_VERSION"),
            helia_gpu::WGPU_VERSION,
        );
        let mut text = text.strip_prefix("helia ").unwrap_or(&text).to_owned();
        if let Some(line) = helia_update::backend::installed().and_then(UpdateBackend::build_line) {
            text.push('\n');
            text.push_str(&line);
        }
        text
    });
    let Some(cli) = parse_cli(long.as_str()) else {
        return Ok(());
    };
    helia_cli::plugins::set_paths(cli.plugin_paths.clone());
    helia_core::io::ffmpeg::set_installed_locator(helia_cli::tools::installed_programs);
    let (program, what) = match &cli.command {
        Command::Stack(_) => ("helia", "stack"),
        Command::Calibrate(_) => ("helia", "calibrate"),
        Command::Edit(_) => ("helia", "edit"),
        Command::Info { .. } => ("helia", "info"),
        Command::Measure { .. } => ("helia", "measure"),
        Command::Sunspots(_) => ("helia", "sunspots"),
        Command::Timelapse(_) => ("helia", "timelapse"),
        Command::SolarDerotate(_) => ("helia", "solar-derotate"),
        Command::Derotate(_) => ("helia", "derotate"),
        Command::Mosaic(_) => ("helia", "mosaic"),
        Command::Plot(_) => ("helia", "plot"),
        Command::Serve(_) => ("helia-server", "serve"),
        Command::Presets => ("helia", "presets"),
        Command::Lut(_) => ("helia", "lut"),
        Command::Feedback(_) => ("helia", "feedback"),
        Command::Plugins(_) => ("helia", "plugins"),
        Command::Update(_) => ("helia", "update"),
        Command::Tools(_) => ("helia", "tools"),
    };
    helia_core::applog::init(program);
    helia_core::applog::info(&format!(
        "{} {what} started",
        helia_core::about::display_version(env!("CARGO_PKG_VERSION"))
    ));
    match cli.command {
        Command::Stack(a) => stack(&a),
        Command::Calibrate(a) => calibrate(&a),
        Command::Edit(a) => edit(&a),
        Command::Info { input, open } => info(&input, &open),
        Command::Measure {
            input,
            crop,
            centre,
            limb,
            psf,
            scale,
            aperture,
            wavelength,
        } => measure(
            &input,
            crop,
            centre,
            (limb, psf),
            scale,
            aperture,
            wavelength,
        ),
        Command::Sunspots(a) => sunspots(&a),
        Command::Timelapse(a) => timelapse(&a),
        Command::SolarDerotate(a) => solar_derotate(&a),
        Command::Derotate(a) => derotate(&a),
        Command::Mosaic(a) => mosaic(&a),
        Command::Plot(a) => plot(&a),
        Command::Serve(a) => serve(a),
        Command::Presets => {
            presets();
            Ok(())
        }
        Command::Lut(a) => lut(&a),
        Command::Feedback(a) => helia_cli::feedback::run(&a),
        Command::Plugins(a) => helia_cli::plugins::run(&a),
        Command::Update(a) => {
            exit_unless_ok(helia_cli::update::run(&a)?);
            Ok(())
        }
        Command::Tools(a) => {
            exit_unless_ok(helia_cli::tools::run(&a)?);
            Ok(())
        }
    }
}

/// Exit with `code` when it is not zero, after flushing what was printed.
fn exit_unless_ok(code: i32) {
    if code != 0 {
        let _ = std::io::stdout().flush();
        std::process::exit(code);
    }
}

fn serve(a: ServeArgs) -> Result<()> {
    let mut b = helia_server::Server::builder()
        .bind(a.bind)
        .data_root(a.data_root.unwrap_or_else(|| PathBuf::from(".")))
        .workers(a.workers)
        .max_queue(a.max_queue);
    if let Some(f) = &a.token_file {
        b = b.token_file(f)?;
    } else if let Some(t) = a.token.or_else(|| std::env::var("HELIA_TOKEN").ok()) {
        b = b.token(t);
    }
    if let Some(d) = a.cache_dir {
        b = b.cache_dir(d);
    }
    for o in a.cors_origin {
        b = b.cors_origin(o);
    }
    for h in a.allow_host {
        b = b.allow_host(h);
    }
    let authenticated = b.config().token.is_some();
    let server = b.build()?;
    let root = server.data_root().to_owned();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    rt.block_on(async {
        let bound = server
            .bind()
            .await
            .with_context(|| format!("cannot bind {}", a.bind))?;
        // Scripts read the first line to learn the port when binding to port 0. A reader that
        // takes it and closes the pipe must not stop the server: `println!` would panic on the
        // broken pipe, so the write errors are ignored.
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "listening on http://{}", bound.local_addr());
            let _ = writeln!(out, "data root: {}", root.display());
            let _ = out.flush();
        }
        helia_core::applog::info(&format!(
            "listening on http://{} ({})",
            bound.local_addr(),
            if authenticated {
                "token set"
            } else {
                "no token"
            }
        ));
        if !authenticated {
            eprintln!(
                "warning: no token set; any local process can read files under the data root"
            );
        }
        bound
            .serve(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

/// What `ffprobe` says about a compressed video, and the quality caveat.
fn video_info(path: &Path) -> Result<()> {
    let i = ffmpeg::inspect(path).with_context(|| format!("cannot read {}", path.display()))?;
    println!("file:        {}", path.display());
    println!(
        "format:      {} (read with ffmpeg)",
        i.container_label(path)
    );
    println!(
        "codec:       {}{}",
        i.codec,
        i.profile
            .as_ref()
            .map_or(String::new(), |p| format!(" ({p})"))
    );
    let (w, h) = i.output_size();
    println!("size:        {w} x {h}");
    println!("pixel format: {}, {} bit", i.pix_fmt, i.bit_depth);
    println!(
        "frames:      {}{}",
        i.frames,
        if i.frames_exact {
            ""
        } else {
            " (estimated from the duration)"
        }
    );
    if let Some(r) = i.frame_rate {
        println!("frame rate:  {r:.3} per second");
    }
    if let Some(d) = i.duration {
        println!("duration:    {d:.1} s");
    }
    if i.rotation != 0 {
        println!("rotation:    {} degrees (applied)", i.rotation);
    }
    if let Some(note) = i.quality_note() {
        println!("note:        {note}");
    }
    Ok(())
}

/// `helia measure`: the granulation contrast of a stacked image, in three bands.
fn measure(
    path: &Path,
    crop: usize,
    centre: Option<(usize, usize)>,
    (limb, psf): (bool, bool),
    scale: Option<f32>,
    aperture: Option<f32>,
    wavelength: Option<f32>,
) -> Result<()> {
    use helia_core::io::any::{Loaded, read_any};
    use helia_core::measure::{centre_crop, crop_around, granulation_contrast_in};
    let (loaded, _) = read_any(path).with_context(|| format!("cannot read {}", path.display()))?;
    let frame = match loaded {
        Loaded::Mono(f) => f,
        Loaded::Rgb(i) => {
            let data = i
                .data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| (p[0] + p[1] + p[2]) / 3.0)
                .collect();
            helia_core::Frame::new(i.width, i.height, data)?
        }
    };
    if limb {
        return limb_report(path, &frame);
    }
    if psf {
        println!("file:        {}", path.display());
        match helia_core::edit::limbpsf::try_measure(&frame) {
            Ok(m) => println!(
                "limb blur:   Gaussian sigma {:.2} px (edge at radius {:.1} px, fit misfit {:.1}% of the step)",
                m.sigma,
                m.radius,
                m.misfit * 100.0
            ),
            Err(why) => println!("limb blur:   not measured: {why}"),
        }
        println!(
            "image scale: Gaussian sigma {:.2} px (what `--rl-psf auto` uses)",
            helia_core::edit::deconv::auto_sigma(&frame)
        );
        return Ok(());
    }
    let area = match centre {
        Some((cx, cy)) => crop_around(&frame, crop, cx, cy),
        None => centre_crop(&frame, crop),
    };
    let b = granulation_contrast_in(&frame, area);
    println!("file:        {}", path.display());
    println!("image:       {} x {}", frame.width, frame.height);
    println!(
        "crop:        {} x {} at ({}, {})",
        area.width(),
        area.height(),
        area.x0,
        area.y0
    );
    println!("fine 0-1 px         {:.3}%", b.fine * 100.0);
    println!("granulation 1-4 px  {:.3}%", b.granulation * 100.0);
    println!("meso 4-12 px        {:.3}%", b.meso * 100.0);
    println!("(band-pass standard deviation over mean; compare only like with like)");
    let optics = helia_core::granulation::Optics {
        aperture_mm: aperture,
        wavelength_nm: wavelength,
        arcsec_per_px: scale,
    };
    println!();
    println!("Granulation report (flattened region, see the user guide)");
    match helia_core::granulation::analyse(&frame, area, &optics) {
        Some(g) => g.lines().iter().for_each(|l| println!("  {l}")),
        None => println!("  the region is too small or has no signal"),
    }
    Ok(())
}

/// Write the outputs of a sunspot detection that were asked for.
fn write_sunspot_files(
    d: &helia_core::sunspots::Detection,
    csv: Option<&Path>,
    json: Option<&Path>,
    overlay: Option<&Path>,
) -> Result<()> {
    let put = |p: &Path, text: &str| {
        std::fs::write(p, text).with_context(|| format!("cannot write {}", p.display()))
    };
    if let Some(p) = csv {
        put(p, &d.report.csv())?;
    }
    if let Some(p) = json {
        put(p, &d.report.json())?;
    }
    if let Some(p) = overlay {
        let f =
            std::fs::File::create(p).with_context(|| format!("cannot write {}", p.display()))?;
        helia_core::io::png::write_png8_rgb(
            std::io::BufWriter::new(f),
            &d.overlay,
            Some("Helia sunspot overlay"),
        )
        .with_context(|| format!("cannot write {}", p.display()))?;
    }
    Ok(())
}

/// `helia sunspots`: sunspots on a stacked image.
fn sunspots(a: &helia_cli::SunspotsArgs) -> Result<()> {
    use helia_core::io::any::{Loaded, read_any};
    use helia_core::io::solarnet::{CaptureInfo, fits_time, unix_from_iso};
    let (loaded, _) =
        read_any(&a.input).with_context(|| format!("cannot read {}", a.input.display()))?;
    let frame = match loaded {
        Loaded::Mono(f) => f,
        Loaded::Rgb(i) => {
            let data = i
                .data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| (p[0] + p[1] + p[2]) / 3.0)
                .collect();
            helia_core::Frame::new(i.width, i.height, data)?
        }
    };
    let when = if let Some(t) = &a.date {
        Some(unix_from_iso(t).with_context(|| {
            format!("cannot read the time {t:?}; use YYYY-MM-DDThh:mm:ss in UTC")
        })?)
    } else if let Some(c) = &a.capture {
        let t = CaptureInfo::read(c, None).mid_unix();
        if t.is_none() {
            bail!(
                "{} has no usable timestamps; give --date instead",
                c.display()
            );
        }
        t
    } else {
        fits_time(&a.input)
    };
    let Some(d) = helia_core::sunspots::detect(&frame, when, &a.tuning.params()) else {
        bail!("no clear solar disc to measure in {}", a.input.display());
    };
    write_sunspot_files(
        &d,
        a.csv.as_deref(),
        a.json.as_deref(),
        a.overlay.as_deref(),
    )?;
    println!("file:        {}", a.input.display());
    println!("image:       {} x {}", frame.width, frame.height);
    println!(
        "disc:        centre ({:.1}, {:.1}), radius {:.1} px",
        d.report.disc.cx, d.report.disc.cy, d.report.disc.radius
    );
    if let Some(t) = d.report.when_unix {
        println!(
            "time:        {} UTC",
            helia_core::io::solarnet::iso_from_unix(t)
        );
    }
    for l in d.report.lines() {
        println!("{l}");
    }
    if !d.report.spots.is_empty() {
        println!();
        print!("{}", sunspot_table(&d.report));
    }
    for n in &d.report.notes {
        println!("note: {n}");
    }
    Ok(())
}

/// Is `path` an image (already stacked) rather than a capture?
fn is_stacked_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "tif" | "tiff" | "png" | "fits" | "fit" | "fts"
        )
    })
}

/// The stacked image for one input: the input itself when it is already an
/// image, else its stack in `dir` (made now, or found there with `reuse`).
fn stack_one(
    input: &Path,
    k: usize,
    total: usize,
    dir: &Path,
    extra: &[String],
    reuse: bool,
) -> Result<PathBuf> {
    if is_stacked_image(input) {
        return Ok(input.to_path_buf());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let stem = input.file_stem().map_or_else(
        || "capture".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    let out = dir.join(format!("{:04}-{stem}.tif", k + 1));
    if reuse && out.is_file() {
        eprintln!(
            "[{}/{total}] {}: using {}",
            k + 1,
            input.display(),
            out.display()
        );
    } else {
        eprintln!("[{}/{total}] stacking {}", k + 1, input.display());
        let sa = helia_cli::stack_args_for(input, &out, extra)
            .map_err(|e| anyhow::anyhow!("the stack settings after `--`: {e}"))?;
        stack(&sa).with_context(|| format!("cannot stack {}", input.display()))?;
    }
    Ok(out)
}

/// `helia mosaic`: stack the panels the same way, match and join them.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: check, stack, join, write, report"
)]
fn mosaic(a: &helia_cli::MosaicArgs) -> Result<()> {
    use helia_core::captime::{TimeHints, capture_time};
    use helia_core::mosaic as mo;
    let started = Instant::now();
    if a.inputs.len() < 2 {
        bail!("a mosaic needs at least two panels");
    }
    helia_core::io::ImageFormat::from_path(&a.output)?;
    if !(a.max_rotation.is_finite() && (0.0..=45.0).contains(&a.max_rotation)) {
        bail!("--max-rotation must be between 0 and 45 degrees");
    }
    if !(0.0..=100.0).contains(&a.min_overlap) || !(0.0..=1.0).contains(&a.min_correlation) {
        bail!("--min-overlap is a percentage and --min-correlation is between 0 and 1");
    }
    if !(a.feather.is_finite() && (0.0..=200.0).contains(&a.feather)) {
        bail!("--feather must be between 0 and 200 pixels");
    }
    let hints = TimeHints {
        date: match &a.date {
            Some(d) => Some(
                helia_core::io::solarnet::unix_from_iso(d)
                    .filter(|_| d.len() == 10)
                    .with_context(|| format!("cannot read the date {d:?}; use YYYY-MM-DD"))?,
            ),
            None => None,
        },
        utc_offset_h: a.utc_offset,
    };
    let derotate = if a.derotate || a.derotate_to.is_some() {
        let target = match a
            .derotate_to
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("middle") => mo::Target::Middle,
            Some("first") => mo::Target::First,
            Some("last") => mo::Target::Last,
            Some(t) => mo::Target::Unix(parse_utc(t)?),
        };
        Some(mo::Derotation {
            law: a.derotation.rotation_law,
            orient: a.derotation.orientation(),
            target,
            preserve_contrast: !a.derotation.raw_brightness,
        })
    } else {
        None
    };
    if !a.stack.is_empty() && a.inputs.iter().all(|p| is_stacked_image(p)) {
        eprintln!(
            "note: every input is already stacked, so the stack settings after `--` are not used"
        );
    }
    let stem = a.output.with_extension("");
    let name = stem
        .file_name()
        .map_or_else(|| "mosaic".to_owned(), |n| n.to_string_lossy().into_owned());
    let stacks_dir = stem.with_file_name(format!("{name}-stacks"));
    let mut stacked = Vec::new();
    let mut times = Vec::new();
    for (k, input) in a.inputs.iter().enumerate() {
        times.push(capture_time(input, &hints).map(|t| t.unix));
        stacked.push(stack_one(
            input,
            k,
            a.inputs.len(),
            &stacks_dir,
            &a.stack,
            a.reuse_stacks,
        )?);
    }
    let opts = mo::Options {
        max_rotation_deg: a.max_rotation,
        min_overlap: a.min_overlap / 100.0,
        min_ncc: a.min_correlation,
        feather: a.feather,
        match_levels: !a.no_gain_match,
        seams: !a.no_seams,
        derotate,
        kernel: Kernel::default(),
    };
    let pictures = stacked
        .iter()
        .map(|p| {
            helia_core::io::any::read_any(p)
                .map(|(l, _)| l)
                .with_context(|| format!("cannot read {}", p.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    eprintln!("joining {} panels", pictures.len());
    let m = mo::build(&pictures, &times, &opts, &|_| {}, &AtomicBool::new(false))
        .context("the mosaic failed")?;
    let notes = helia_core::io::ImageNotes {
        lines: mo::summary_lines(&m)
            .into_iter()
            .map(|l| format!("Helia mosaic: {l}"))
            .collect(),
        fits: None,
    };
    if let Some(dir) = a.output.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    helia_core::picture::save(&a.output, &m.image, &notes)
        .with_context(|| format!("cannot write {}", a.output.display()))?;
    let names: Vec<String> = a
        .inputs
        .iter()
        .map(|p| {
            p.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        })
        .collect();
    let panels_csv = stem.with_file_name(format!("{name}-panels.csv"));
    let pairs_csv = stem.with_file_name(format!("{name}-pairs.csv"));
    std::fs::write(&panels_csv, mo::panels_csv(&m, &names))
        .with_context(|| format!("cannot write {}", panels_csv.display()))?;
    std::fs::write(&pairs_csv, mo::pairs_csv(&m))
        .with_context(|| format!("cannot write {}", pairs_csv.display()))?;
    for l in mo::summary_lines(&m) {
        println!("{l}");
    }
    for p in &m.panels {
        println!(
            "  panel {} {}  at ({:>8.2}, {:>8.2}) px  turn {:>+6.3} deg  gain {:.3} offset {:+.4}{}",
            p.index + 1,
            names[p.index],
            p.x,
            p.y,
            p.angle_deg,
            p.gain,
            p.offset,
            p.derotated_px
                .map_or_else(String::new, |d| format!("  surface moved up to {d:.1} px"))
        );
    }
    println!(
        "wrote {} ({}, {})",
        a.output.display(),
        panels_csv.display(),
        pairs_csv.display()
    );
    eprintln!("done in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}

/// `helia timelapse`: stack every capture the same way, register the stacks
/// to each other and write the frames and the movie.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: order, stack, register, report"
)]
fn timelapse(a: &helia_cli::TimelapseArgs) -> Result<()> {
    use helia_core::captime::{TimeHints, capture_time};
    use helia_core::timelapse as tl;
    let started = Instant::now();
    let hints = TimeHints {
        date: match &a.date {
            Some(d) => Some(
                helia_core::io::solarnet::unix_from_iso(d)
                    .filter(|_| d.len() == 10)
                    .with_context(|| format!("cannot read the date {d:?}; use YYYY-MM-DD"))?,
            ),
            None => None,
        },
        utc_offset_h: a.utc_offset,
    };
    if !(a.fps.is_finite() && a.fps > 0.0 && a.fps <= 120.0) {
        bail!("--fps must be between 0 and 120");
    }
    if !(a.max_refine.is_finite() && a.max_refine >= 0.0) {
        bail!("--max-refine must not be negative");
    }
    let reference = match a.reference.parse::<tl::Reference>()? {
        tl::Reference::Index(0) => bail!("--reference counts frames from 1"),
        tl::Reference::Index(n) => tl::Reference::Index(n - 1),
        other => other,
    };
    let deflicker = match a.deflicker {
        helia_cli::DeflickerArg::Off => tl::Deflicker::Off,
        helia_cli::DeflickerArg::Match => tl::Deflicker::Match,
        helia_cli::DeflickerArg::Smooth => {
            if a.deflicker_window < 3 || a.deflicker_window.is_multiple_of(2) {
                bail!("--deflicker-window must be odd and at least 3");
            }
            tl::Deflicker::Smooth(a.deflicker_window)
        }
    };
    let derotate = if a.derotate || a.derotate_to.is_some() {
        let target = match &a.derotate_to {
            Some(t) => tl::DerotateTarget::Unix(parse_utc(t)?),
            None => tl::DerotateTarget::Reference,
        };
        Some(a.derotation.derotation(target))
    } else {
        None
    };
    // Order by time when every input has one.
    let found: Vec<_> = a.inputs.iter().map(|p| capture_time(p, &hints)).collect();
    let unix: Vec<Option<f64>> = found.iter().map(|t| t.as_ref().map(|t| t.unix)).collect();
    let order: Vec<usize> = if a.no_sort {
        (0..a.inputs.len()).collect()
    } else {
        tl::order_by_time(&unix)
    };
    if order.windows(2).any(|w| w[0] > w[1]) {
        eprintln!("note: the captures were not in time order; they are sorted by their times");
    }
    if !a.stack.is_empty() && a.inputs.iter().all(|p| is_stacked_image(p)) {
        eprintln!(
            "note: every input is already stacked, so the stack settings after `--` are not used"
        );
    }
    let stacks_dir = a.output.join("stacks");
    let mut stacked = Vec::new();
    let mut times = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        let input = &a.inputs[i];
        let when = found[i].clone();
        if when.is_none() {
            eprintln!(
                "note: no time for {}; give --date for a name that holds only the time of day",
                input.display()
            );
        }
        let path = stack_one(input, k, order.len(), &stacks_dir, &a.stack, a.reuse_stacks)?;
        stacked.push(path);
        times.push(when.map(|t| t.unix));
    }
    let opts = tl::Options {
        reference,
        refine: !a.no_refine,
        max_refine_px: a.max_refine,
        deflicker,
        crop: a.crop,
        kernel: Kernel::default(),
        derotate,
    };
    let out = tl::Output {
        dir: a.output.clone(),
        format: a.format,
        movie: a.movie,
        fps: a.fps,
    };
    eprintln!("registering {} frames", stacked.len());
    let outcome = tl::run(
        &stacked,
        &times,
        &opts,
        &out,
        &|_| {},
        &AtomicBool::new(false),
    )
    .context("the time-lapse failed")?;
    for l in tl::summary_lines(&outcome.sequence) {
        println!("{l}");
    }
    for r in &outcome.sequence.report {
        let when = r.time.map_or_else(
            || "-".to_owned(),
            |t| helia_core::io::solarnet::iso_from_unix(t)[11..19].to_owned(),
        );
        let phase = r
            .phase
            .map_or_else(String::new, |(_, psr)| format!("  phase PSR {psr:.0}"));
        println!(
            "  {:>3} {when}  shift ({:>6.2}, {:>6.2}) px  {}{phase}",
            r.index + 1,
            r.shift.dx,
            r.shift.dy,
            r.method.label()
        );
    }
    let failed = outcome.sequence.failed();
    if !failed.is_empty() {
        eprintln!(
            "warning: no registration for frame(s) {}: no clear solar limb and no phase lock; they are left as they are",
            failed
                .iter()
                .map(|i| (i + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "wrote {} frames to {} ({})",
        outcome.files.len(),
        a.output.display(),
        outcome.table.display()
    );
    match &outcome.movie {
        Some(m) => {
            if let Some(n) = &m.note {
                eprintln!("note: {n}");
            }
            println!("wrote {} ({})", m.path.display(), m.kind);
        }
        None => println!("no movie (needs two or more frames and --movie other than none)"),
    }
    if let Some(dir) = &a.report {
        let data = helia_report::ScienceData::from_sequence(&outcome.sequence);
        let meta = helia_report::html::Meta {
            title: format!("time-lapse of {} captures", a.inputs.len()),
            inputs: vec![("output".into(), a.output.display().to_string())],
            settings: vec![
                ("reference".into(), format!("{:?}", opts.reference)),
                (
                    "refine by phase correlation".into(),
                    opts.refine.to_string(),
                ),
                ("deflicker".into(), format!("{:?}", opts.deflicker)),
                ("derotation".into(), format!("{:?}", opts.derotate)),
            ],
            generated: helia_core::io::solarnet::iso_from_unix(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64()),
            ),
            version: helia_core::about::display_version(env!("CARGO_PKG_VERSION")),
        };
        let bundle =
            helia_report::generate(&data, &meta, None, helia_report::canvas::Size::default())
                .context("cannot make the diagnostics")?;
        bundle
            .write(dir)
            .with_context(|| format!("cannot write the diagnostics to {}", dir.display()))?;
        for s in &bundle.sections {
            for n in &s.notes {
                eprintln!("report:  not drawn: {n}");
            }
        }
        eprintln!(
            "report:  {} files in {} (open {})",
            bundle.files.len(),
            dir.display(),
            dir.join(helia_report::REPORT_HTML).display()
        );
    }
    eprintln!("done in {:.1} s", started.elapsed().as_secs_f32());
    Ok(())
}

/// A UTC time as unix seconds, from `YYYY-MM-DDThh:mm:ss`.
fn parse_utc(t: &str) -> Result<f64> {
    helia_core::io::solarnet::unix_from_iso(t)
        .with_context(|| format!("cannot read the time {t:?}; use YYYY-MM-DDThh:mm:ss in UTC"))
}

/// `helia solar-derotate`: bring images of the Sun to one time.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: times, target, derotate, write"
)]
fn solar_derotate(a: &helia_cli::SolarDerotateArgs) -> Result<()> {
    use helia_core::captime::{TimeHints, capture_time};
    use helia_core::derotate::{Spec, derotate, disc_of};
    use helia_core::io::any::read_any;
    let hints = TimeHints {
        date: match &a.date {
            Some(d) => Some(
                helia_core::io::solarnet::unix_from_iso(d)
                    .filter(|_| d.len() == 10)
                    .with_context(|| format!("cannot read the date {d:?}; use YYYY-MM-DD"))?,
            ),
            None => None,
        },
        utc_offset_h: a.utc_offset,
    };
    let mut times = Vec::new();
    for p in &a.inputs {
        match capture_time(p, &hints) {
            Some(t) => times.push(t.unix),
            None => bail!(
                "no time for {}; give --date for a name that holds only the time of day, or a FITS file with DATE-AVG",
                p.display()
            ),
        }
    }
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let target = match a.to.to_ascii_lowercase().as_str() {
        "first" => sorted[0],
        "last" => sorted[sorted.len() - 1],
        "middle" => sorted[sorted.len() / 2],
        _ => parse_utc(&a.to)?,
    };
    std::fs::create_dir_all(&a.output)
        .with_context(|| format!("cannot create {}", a.output.display()))?;
    println!(
        "bringing {} image(s) to {} UTC, {} rates",
        a.inputs.len(),
        helia_core::io::solarnet::iso_from_unix(target).replace('T', " ")[..19].to_owned(),
        a.tuning.rotation_law.label()
    );
    for (p, &t) in a.inputs.iter().zip(&times) {
        let (pic, _) = read_any(p).with_context(|| format!("cannot read {}", p.display()))?;
        let Some(disc) = disc_of(&pic) else {
            bail!(
                "no clear solar disc in {}; the whole disc must be in view",
                p.display()
            );
        };
        let spec = Spec {
            law: a.tuning.rotation_law,
            orient: a.tuning.orientation(),
            from_unix: t,
            to_unix: target,
            preserve_contrast: !a.tuning.raw_brightness,
            kernel: Kernel::default(),
        };
        let (out, rep) = derotate(&pic, disc, &spec)?;
        let stem = p
            .file_stem()
            .map_or_else(|| "image".to_owned(), |s| s.to_string_lossy().into_owned());
        let ext = p
            .extension()
            .map_or_else(|| "tif".to_owned(), |s| s.to_string_lossy().into_owned());
        let path = a.output.join(format!("{stem}-derotated.{ext}"));
        let note = format!(
            "Derotated by Helia from {} to {} UTC: differential rotation, {} rates, surface moved by up to {:.1} px",
            helia_core::io::solarnet::iso_from_unix(t).replace('T', " ")[..19].to_owned(),
            helia_core::io::solarnet::iso_from_unix(target).replace('T', " ")[..19].to_owned(),
            a.tuning.rotation_law.label(),
            rep.max_shift_px
        );
        helia_core::picture::save(&path, &out, &helia_core::io::ImageNotes::line(note))
            .with_context(|| format!("cannot write {}", path.display()))?;
        println!(
            "  {}  {} UTC  {:+.1} min  disc ({:.1}, {:.1}) r {:.1} px  moved up to {:.2} px (centre {:.2})  -> {}",
            p.display(),
            &helia_core::io::solarnet::iso_from_unix(t)[11..19],
            rep.dt_s / 60.0,
            disc.cx,
            disc.cy,
            disc.r,
            rep.max_shift_px,
            rep.centre_shift_px,
            path.display()
        );
    }
    Ok(())
}

/// The spots as an aligned text table.
fn sunspot_table(r: &helia_core::sunspots::Report) -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "spot group    x     y    lat   cmd   lon    mu umbrae  as seen corrected umbra (MSH)
",
    );
    for p in &r.spots {
        let lon = p.lon_deg.map_or("    -".to_owned(), |l| format!("{l:5.1}"));
        let _ = writeln!(
            s,
            "{:>4} {:>5} {:>5.0} {:>5.0} {:>+5.1} {:>+5.1} {lon} {:>5.2} {:>6} {:>8.0} {:>9.0} {:>11.0}",
            p.id,
            p.group,
            p.x,
            p.y,
            p.lat_deg,
            p.cmd_deg,
            p.mu,
            p.umbrae,
            p.projected_msh,
            p.area_msh,
            p.umbra_msh
        );
    }
    s
}

/// `helia stack --sunspots`: detect on the stack, save the table, JSON and
/// overlay beside the output, and print the report.
fn stack_sunspots(
    a: &StackArgs,
    out: &helia_core::pipeline::StackOutput,
    info: &CaptureInfo,
    quiet: bool,
) -> Option<helia_core::sunspots::Detection> {
    let when = a.date.or_else(|| {
        FitsMeta::from_stack(info, out, None)
            .date_avg
            .or_else(|| info.mid_unix())
    });
    let Some(d) = helia_core::sunspots::detect(&out.image, when, &a.sunspot.params()) else {
        eprintln!("sunspots: no clear solar disc to measure");
        return None;
    };
    let stem = a.output.with_extension("");
    let name = stem
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let beside = |ext: &str| stem.with_file_name(format!("{name}-sunspots.{ext}"));
    if let Err(e) = write_sunspot_files(
        &d,
        Some(&beside("csv")),
        Some(&beside("json")),
        Some(&beside("png")),
    ) {
        eprintln!("sunspots: {e:#}");
    }
    if !quiet {
        for l in d.report.lines() {
            eprintln!("sunspots: {l}");
        }
        for n in &d.report.notes {
            eprintln!("sunspots: note: {n}");
        }
        eprintln!(
            "sunspots: wrote {}-sunspots.csv, .json and .png beside the output",
            stem.display()
        );
    }
    Some(d)
}

/// `helia derotate`: turn stacked images of one session to one orientation.
#[allow(
    clippy::too_many_lines,
    reason = "read, plan, turn and write, in order"
)]
fn derotate(a: &helia_cli::DerotateArgs) -> Result<()> {
    use helia_core::field_rotation::{
        FieldRotation, Reference, rotate_ccw, sun_parallactic_angle_deg,
    };
    use helia_core::io::any::{Loaded, read_any};
    use helia_core::io::solarnet::{fits_time, iso_from_unix, unix_from_iso};
    use helia_core::output::write_stack;
    let site = a.site.or_else(Site::from_env).context(
        "helia derotate needs the observing site: give --site LAT,LON,ALT or set HELIA_SITE",
    )?;
    let n = a.inputs.len();
    if a.times.len() > n {
        bail!("{} times for {n} images", a.times.len());
    }
    let mut times = Vec::with_capacity(n);
    for (i, p) in a.inputs.iter().enumerate() {
        let t = match a.times.get(i).filter(|t| !t.trim().is_empty()) {
            Some(t) => Some(unix_from_iso(t).with_context(|| {
                format!("cannot read the time {t:?}; use YYYY-MM-DDThh:mm:ss in UTC")
            })?),
            None => fits_time(p),
        };
        let Some(t) = t else {
            bail!(
                "{} has no time: give it with --times (a FITS file's DATE-AVG or DATE-OBS is used when there is one)",
                p.display()
            );
        };
        times.push(t);
    }
    let reference = match a.reference.trim().parse::<usize>() {
        Ok(k) if (1..=n).contains(&k) => Some(k - 1),
        Ok(k) => bail!("--reference {k}: there are {n} images"),
        Err(_) => None,
    };
    let rule: Reference = if reference.is_some() {
        Reference::First
    } else {
        a.reference.parse()?
    };
    let mut plan = FieldRotation {
        site,
        mirror: a.mirror,
        reference: rule,
        target: None,
        auto_mirror: false,
        frame_unix: times.iter().map(|&t| Some(t)).collect(),
    }
    .plan()?;
    if let Some(k) = reference {
        // The named image is the reference: measure every turn against it.
        let sign = if a.mirror { -1.0 } else { 1.0 };
        let base = plan.turn_deg[k] / sign;
        for t in &mut plan.turn_deg {
            *t -= sign * base;
        }
        plan.reference_unix = times[k];
        plan.max_turn_deg = plan.turn_deg.iter().map(|t| t.abs()).fold(0.0, f64::max);
    }
    std::fs::create_dir_all(&a.output_dir)
        .with_context(|| format!("cannot create {}", a.output_dir.display()))?;
    let backend = select_backend(helia_gpu::BackendChoice::Cpu)?;
    println!(
        "{:<28} {:<24} {:>10} {:>9}",
        "image", "time (UTC)", "q (deg)", "turn (deg)"
    );
    for (i, p) in a.inputs.iter().enumerate() {
        let (image, _) = read_any(p).with_context(|| format!("cannot read {}", p.display()))?;
        let turn = plan.turn_deg[i];
        let turned = match &image {
            Loaded::Mono(f) => Loaded::Mono(rotate_ccw(f, turn, Kernel::default())),
            Loaded::Rgb(rgb) => {
                let plane = |c: usize| {
                    helia_core::Frame::new(
                        rgb.width,
                        rgb.height,
                        rgb.data.as_chunks::<3>().0.iter().map(|px| px[c]).collect(),
                    )
                };
                let planes = [plane(0)?, plane(1)?, plane(2)?]
                    .map(|f| rotate_ccw(&f, turn, Kernel::default()));
                Loaded::Rgb(helia_core::RgbImage {
                    width: rgb.width,
                    height: rgb.height,
                    data: (0..rgb.width * rgb.height)
                        .flat_map(|k| [planes[0].data[k], planes[1].data[k], planes[2].data[k]])
                        .collect(),
                })
            }
        };
        let name = p
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let ext = p
            .extension()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let out = a.output_dir.join(format!("{name}-derot.{ext}"));
        if out.exists() && !a.overwrite {
            bail!("{} exists; add --overwrite", out.display());
        }
        let note = format!(
            "Field rotation corrected: turned {turn:+.3} degrees counter-clockwise to the orientation at {} (altazimuth parallactic angle of the Sun at {}, site {:.3},{:.3}{})",
            iso_from_unix(plan.reference_unix),
            iso_from_unix(times[i]),
            site.lat_deg,
            site.lon_deg,
            if a.mirror { ", mirror image" } else { "" }
        );
        let lead = vec![
            note,
            format!("DATE-AVG of the original: {}", iso_from_unix(times[i])),
        ];
        let is_fits = matches!(ImageFormat::from_path(&out), Ok(ImageFormat::Fits));
        match (&turned, is_fits) {
            (Loaded::Mono(f), true) => {
                let plan = OutputPlan {
                    edits: helia_core::EditList::default(),
                    colour: ColourParams::default(),
                    edit_fits: true,
                    lead_notes: lead,
                    fits: Some(FitsMeta::derotated(
                        times[i],
                        site,
                        turn,
                        plan.reference_unix,
                    )),
                    rgb: None,
                };
                write_stack(backend.as_ref(), f, &out, &plan)
                    .map(|_| ())
                    .with_context(|| format!("cannot write {}", out.display()))?;
            }
            _ => {
                write_loaded(
                    backend.as_ref(),
                    &turned,
                    &out,
                    &helia_core::EditList::default(),
                    &lead,
                )
                .map(|_| ())
                .with_context(|| format!("cannot write {}", out.display()))?;
            }
        }
        println!(
            "{:<28} {:<24} {:>10.3} {:>+9.3}",
            p.file_name()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            iso_from_unix(times[i]),
            sun_parallactic_angle_deg(&site, times[i]),
            turn
        );
    }
    for l in plan.lines(None) {
        println!("{l}");
    }
    println!("wrote:   {} images to {}", n, a.output_dir.display());
    Ok(())
}

/// `helia measure --limb`: the laws fitted to the disc, and the profile they
/// are fitted to.
fn limb_report(path: &Path, frame: &helia_core::Frame) -> Result<()> {
    use helia_core::limbdark::{self, Law};
    let Some(first) = limbdark::fit_image(frame, Law::Quadratic) else {
        bail!("no clear solar disc to fit in {}", path.display());
    };
    println!("file:        {}", path.display());
    println!("image:       {} x {}", frame.width, frame.height);
    println!(
        "disc:        centre ({:.1}, {:.1}), radius {:.1} px, sky {:.4}, centre {:.4}",
        first.cx, first.cy, first.radius, first.background, first.centre
    );
    let fits: Vec<_> = Law::ALL
        .into_iter()
        .filter_map(|l| limbdark::fit_with_disc(frame, first.disc(), l))
        .collect();
    for f in &fits {
        println!("{}", f.summary());
    }
    let profile = limbdark::radial_profile(frame, first.disc());
    print!("mu    measured ");
    for f in &fits {
        print!(" {:>8}", f.law.id());
    }
    println!();
    for mu in [1.0f32, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.25] {
        let x = (1.0 - mu * mu).sqrt();
        let i = ((x * profile.len() as f32) as usize).min(profile.len() - 1);
        let Some(v) = profile[i] else { continue };
        print!("{mu:<5.2} {:<9.4}", (v - first.background) / first.centre);
        for f in &fits {
            print!(" {:>8.4}", f.level(mu));
        }
        println!();
    }
    println!(
        "(the rings are an azimuthal median: whatever is radial in the image, a vignette included, reads as limb darkening)"
    );
    Ok(())
}

/// `helia info`: the header as written, what Helia will use and why, and
/// anything it wants the user to know.
fn info(path: &Path, open: &OpenArgs) -> Result<()> {
    if ffmpeg::is_video_path(path) {
        return video_info(path);
    }
    let report = match helia_core::io::capture::inspect(path, &open.options()) {
        Ok(r) => r,
        // Compressed AVI (MJPEG, H.264, ...) goes to ffmpeg.
        Err(helia_core::Error::Avi(AviError::UnsupportedFormat(_))) => return video_info(path),
        Err(e) => return Err(e).with_context(|| format!("cannot open {}", path.display())),
    };
    println!("{:<19} {}", "file:", path.display());
    println!("{:<19} {}", "format:", report.container);
    println!();
    println!("Header, as written in the file");
    for (label, value) in report.header_rows() {
        println!("  {:<18} {value}", format!("{label}:"));
    }
    println!();
    println!("Helia will use");
    for (label, value, origin) in report.effective_rows() {
        println!(
            "  {:<18} {value:<14} ({})",
            format!("{label}:"),
            origin.label()
        );
    }
    if let Some(d) = &report.detection {
        println!();
        println!("Mosaic check (2x2 pixel positions, share of full scale)");
        println!(
            "  {:<18} {}",
            "means:",
            d.means.map(|m| format!("{:.1}%", m * 100.0)).join("  ")
        );
        println!(
            "  {:<18} {:.1}% over {} frame(s)",
            "spread:",
            d.spread * 100.0,
            d.frames_used
        );
    }
    if !report.notices.is_empty() {
        println!();
    }
    for line in notice_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

/// The report's notices as lines, tagged by severity.
fn notice_lines(report: &CaptureReport) -> Vec<String> {
    use helia_core::io::capture::Level;
    report
        .notices
        .iter()
        .map(|n| {
            let tag = match n.level {
                Level::Warning => "warning:",
                Level::Info => "note:",
            };
            format!("{tag:<9} {}", n.text)
        })
        .collect()
}

/// The combination the robust options ask for; the weighted mean when none.
fn stack_mode(a: &StackArgs) -> StackMode {
    let kind = match a.robust {
        Some(RobustArg::Sigma) => RobustKind::Sigma,
        Some(RobustArg::Huber) => RobustKind::Huber,
        Some(RobustArg::Winsor) => RobustKind::Winsor,
        Some(RobustArg::None) => return StackMode::Mean,
        None => {
            return a
                .sigma_clip
                .map_or(StackMode::Mean, |kappa| StackMode::SigmaClip { kappa });
        }
    };
    StackMode::from_kind(kind, a.robust_k.unwrap_or_else(|| kind.default_k()))
}

/// How many frames the run keeps: the option when given, else the
/// recommendation.
fn keep_used(a: &StackArgs, rec: &Recommendation) -> Keep {
    match (a.keep, a.keep_frames) {
        (_, Some(n)) => Keep::Count(n),
        (Some(k), None) => k,
        (None, None) => rec.keep,
    }
}

/// The recommendation with every setting the user gave in place of its own,
/// so the `settings:` line says what the run uses.
fn recommendation_used(a: &StackArgs, rec: &Recommendation) -> Recommendation {
    Recommendation {
        keep: keep_used(a, rec),
        ap_size: a.ap_size.unwrap_or(rec.ap_size),
        channel: a.channel.map_or(rec.channel, |c| match c {
            ChannelArg::R => Channel::Red,
            ChannelArg::G => Channel::Green,
            ChannelArg::B => Channel::Blue,
            ChannelArg::Lum => Channel::Luminance,
        }),
        ..rec.clone()
    }
}

/// Validate the arguments and turn them into a pipeline configuration.
/// `rec` supplies whatever the user left unsaid.
#[allow(
    clippy::too_many_lines,
    reason = "one validation sequence over every stack flag"
)]
fn build_config(a: &StackArgs, rec: &Recommendation) -> Result<StackConfig> {
    if a.keep_frames == Some(0) {
        bail!("--keep-frames must be at least 1");
    }
    let keep = keep_used(a, rec);
    if a.sigma_clip.is_some_and(|k| !(k.is_finite() && k > 0.0)) {
        bail!("--sigma-clip must be a positive number of standard deviations");
    }
    if a.robust_k.is_some_and(|k| !(k.is_finite() && k > 0.0)) {
        bail!("--robust-k must be a positive number of standard deviations");
    }
    let ap_size = a.ap_size.unwrap_or(rec.ap_size);
    if !(0.0..=100.0 * helia_core::autocrop::MAX_MARGIN).contains(&a.autocrop_margin) {
        bail!(
            "--autocrop-margin must be between 0 and {} percent",
            100.0 * helia_core::autocrop::MAX_MARGIN
        );
    }
    // Fail on a bad output name before spending minutes stacking.
    helia_core::io::ImageFormat::from_path(&a.output)?;
    let true_colour = a.colour == Some(ColourChoice::True);
    if true_colour {
        if !rec.is_bayer {
            bail!(
                "--colour true needs a raw colour capture (a Bayer mosaic); this capture is mono or already demosaiced. Use --colour mono, or a preset for a filter"
            );
        }
        if a.grade.is_some() {
            bail!("--grade does not apply to --colour true: it has no colour look to grade");
        }
        if a.channel.is_some() || a.invert || (a.colour_strength - 1.0).abs() > f32::EPSILON {
            bail!(
                "--channel, --invert and --colour-strength do not apply to --colour true: it stacks all three channels and has no colour look"
            );
        }
    } else if a.white_balance != WhiteBalance::Auto || a.no_channel_align {
        bail!("--white-balance and --no-channel-align belong to --colour true; add it");
    }
    Ok(StackConfig {
        field_rotation: field_rotation(a, rec)?,
        true_colour: true_colour.then_some(TrueColour {
            white_balance: a.white_balance,
            align_channels: !a.no_channel_align,
        }),
        keep,
        metric: match a.metric {
            MetricArg::Gradient => Metric::GradientEnergy,
            MetricArg::Laplacian => Metric::LaplacianVariance,
            MetricArg::Helmli => Metric::HelmliScherer,
            MetricArg::Mfgs => Metric::Mfgs,
            MetricArg::Dct => Metric::DctEnergyRatio,
            MetricArg::Fourier => Metric::FourierBand,
            MetricArg::Contrast => Metric::LocalContrast,
        },
        multipoint: (!a.no_multipoint).then(|| MultipointConfig {
            ap_size,
            ..MultipointConfig::default()
        }),
        mode: stack_mode(a),
        kernel: Kernel::default(),
        auto_ap: a
            .ap_auto
            .then(|| AutoAp::for_preset(rec.preset, rec.ap_size)),
        autocrop: a.autocrop.then_some(a.autocrop_margin / 100.0),
        limb_law: Some(a.edits.limb_law.unwrap_or_default()),
        ap_refine: ApRefine {
            passes: a.ap_passes.unwrap_or(ApRefine::default().passes),
            smooth: match a.ap_smooth {
                Some(OnOff::On) => Some(SmoothConfig::default()),
                Some(OnOff::Off) => None,
                None => ApRefine::default().smooth,
            },
            residual: a.ap_residual,
        },
        seeing: (a.seeing || a.report.is_some()).then(|| helia_core::seeing::SeeingSettings {
            aperture_mm: a.aperture_mm,
            wavelength_nm: a
                .wavelength_nm
                .or_else(|| rec.preset.map(helia_core::seeing::preset_wavelength_nm)),
        }),
        align: a.align,
        debayer: Debayer {
            method: match a.debayer {
                DebayerArg::Bilinear => Method::Bilinear,
                DebayerArg::Mhc => Method::Mhc,
            },
            channel: a.channel.map_or(rec.channel, |c| match c {
                ChannelArg::R => Channel::Red,
                ChannelArg::G => Channel::Green,
                ChannelArg::B => Channel::Blue,
                ChannelArg::Lum => Channel::Luminance,
            }),
        },
        // A plane named with --channel is the user's choice; a preset's can move.
        clip_fallback: a.channel.is_none(),
        mask: MaskConfig {
            disc: !a.no_disc_mask,
            prominence_band: a.prominence_band,
            overlay: !a.no_overlay_detect,
            exclude: a.exclude.clone(),
        },
        ..StackConfig::default()
    })
}

/// The colouring `--preset`, `--colour`, `--invert` and `--colour-strength`
/// ask for, given what was detected. A `None` preset is plain mono.
/// The field rotation correction for this stack: the one `--field-rotation`
/// asks for, or, for FITS subs from an altazimuth mount (a Seestar), the one
/// their headers call for unless `--no-field-rotation` says not to.
///
/// A header gives the target (`RA`, `DEC`), the site (`SITELAT`, `SITELONG`)
/// and the mount (`EQMODE` 0, altazimuth); with the time of each frame that is
/// the parallactic angle of every frame (see `helia_core::field_rotation`).
/// Without `--field-rotation` it is applied only when the headers hold all of
/// that and the field turns by more than a hundredth of a degree.
fn field_rotation(
    a: &StackArgs,
    rec: &Recommendation,
) -> Result<Option<helia_core::field_rotation::FieldRotation>> {
    use helia_core::field_rotation::{FieldRotation, SkyPoint};
    if a.no_field_rotation {
        return Ok(None);
    }
    let info = CaptureInfo::read(&a.input, rec.preset);
    let timed = info.frame_unix.iter().any(Option::is_some);
    let header = info
        .pointing
        .filter(helia_core::io::fitsseq::Pointing::is_altazimuth);
    if !a.field_rotation && (!timed || header.and_then(|p| p.site()).is_none()) {
        return Ok(None);
    }
    // The header site serves a sub, but a site the user gave wins.
    let site = a
        .site
        .or_else(|| header.and_then(|p| p.site()))
        .or_else(Site::from_env)
        .context(
            "--field-rotation needs the observing site: give --site LAT,LON,ALT or set HELIA_SITE",
        )?;
    if !timed {
        bail!(
            "--field-rotation needs the time of each frame, and {} has none (SER files and FITS frames with a DATE-OBS carry them; AVI and video do not)",
            a.input.display()
        );
    }
    let rotation = FieldRotation {
        site,
        mirror: a.field_rotation_mirror,
        reference: a.field_rotation_reference,
        // Subs name their target: which way the picture turns is then read
        // from the stars, unless --field-rotation-mirror says.
        auto_mirror: info.pointing.is_some() && !a.field_rotation_mirror,
        target: info.pointing.map(|p| SkyPoint {
            ra_deg: p.ra_deg,
            dec_deg: p.dec_deg,
        }),
        frame_unix: info.frame_unix,
    };
    if !a.field_rotation {
        // Found from the headers: keep it only when the field really turns.
        return Ok(rotation
            .plan()
            .ok()
            .filter(|p| p.max_turn_deg >= 0.01)
            .map(|_| rotation));
    }
    Ok(Some(rotation))
}

/// The job a stack-backend plugin gets: the API job vocabulary, built from what this run resolved.
/// Settings a backend on another computer cannot honour are refused here, in plain words.
fn backend_job(
    a: &StackArgs,
    cfg: &StackConfig,
    calibration: &[String],
    filter: Option<&str>,
) -> Result<serde_json::Value> {
    if !calibration.is_empty() {
        bail!(
            "Calibration masters cannot be used by a stack-backend plugin yet. What to do: leave out the dark, flat and bad-pixel options, or stack on this computer."
        );
    }
    let job =
        helia_plugin::job::stack_job(cfg, &a.open.options(), filter).map_err(anyhow::Error::msg)?;
    // The job must be one the API itself would accept.
    let parsed: helia_server::jobs::JobConfig = serde_json::from_value(job.clone())
        .context("the stacking settings do not fit the job format a plugin expects")?;
    parsed.resolve().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(job)
}

/// Where the `analyse` steps write their result: next to the output image.
fn analysis_path(output: &Path) -> PathBuf {
    let mut name = output.file_stem().map_or_else(
        || std::ffi::OsString::from("output"),
        std::ffi::OsString::from,
    );
    name.push(".analysis.json");
    output.with_file_name(name)
}

fn colour_params(a: &StackArgs, rec: &Recommendation) -> Result<ColourParams> {
    if !(0.0..=1.0).contains(&a.colour_strength) {
        bail!("--colour-strength must be between 0 and 1");
    }
    let preset = match (a.colour, a.preset) {
        (Some(ColourChoice::Preset(p)), _) | (None, Some(p)) => Some(p),
        (Some(ColourChoice::Auto), _) => Some(rec.preset.context(
            "--colour auto needs a filter: none was recognised in the file name, folder or \
             sidecar. Give --preset or --colour <filter>",
        )?),
        (Some(ColourChoice::Mono | ColourChoice::True), _) | (None, None) => None,
    };
    let grade = match a.grade.as_deref() {
        None => helia_core::grade::Grade::default(),
        Some(text) if text.contains('=') || text.is_empty() => {
            helia_core::grade::Grade::parse(text)?
        }
        Some(name) => {
            helia_core::grade::find(name, helia_core::grade::presets_dir().as_deref())?.grade
        }
    };
    Ok(ColourParams {
        preset,
        invert: a.invert,
        strength: a.colour_strength,
        grade,
    })
}

/// `helia presets`: id, name, wavelength and what each does.
fn presets() {
    println!(
        "{:<7} {:<13} {:<17} WHAT IT DOES",
        "ID", "NAME", "WAVELENGTH"
    );
    for p in Preset::ALL {
        let i = p.info();
        println!(
            "{:<7} {:<13} {:<17} {}",
            i.id, i.name, i.wavelength, i.summary
        );
    }
    println!();
    println!("Use `helia stack <capture> -o out.tif --preset <id>` for the recommended channel,");
    println!("frame share and colour, or leave it out to detect the filter from the file name.");
}

/// `helia lut`: write a preset's colour table.
fn lut(a: &helia_cli::LutArgs) -> Result<()> {
    use helia_core::lut_export::{LutFormat, export};
    let format = match a.format {
        Some(f) => f,
        None => LutFormat::from_path(&a.output).with_context(|| {
            format!(
                "cannot tell the format from {}; use .cube, .csv, .lut or .png, or --format",
                a.output.display()
            )
        })?,
    };
    let params = ColourParams {
        preset: Some(a.preset),
        invert: a.invert,
        strength: a.strength.clamp(0.0, 1.0),
        ..ColourParams::default()
    };
    let title = format!("Helia {}", a.preset.name());
    let bytes = export(&params.lut(), &title, format, a.size)?;
    std::fs::write(&a.output, bytes)
        .with_context(|| format!("cannot write {}", a.output.display()))?;
    println!("{}", params.describe());
    println!(
        "wrote:   {} ({}, {} entries)",
        a.output.display(),
        format.id(),
        if format == LutFormat::ImageJ {
            256
        } else {
            a.size
        }
    );
    Ok(())
}

/// A stage line on stderr that does not tear the progress bar.
fn stage_line(bar: &ProgressBar, started: Instant, text: &str) {
    bar.suspend(|| eprintln!("[{:>6.1}s] {text}", started.elapsed().as_secs_f32()));
}

/// A job started on another thread that is always finished before the owner goes away.
///
/// `helia stack` starts the GPU while the capture is opened. An error on the way (a bad
/// option, a file that will not open) used to return from `run` and `exit` the process
/// with that thread still inside the Vulkan driver's instance creation. `exit` runs the
/// driver libraries' atexit handlers, which take locks the starting thread holds, and the
/// process hung for good: two threads on two futexes, found with a stack scan of a hung
/// child on a loaded machine (issue #185). Dropping this waits for the job, so the
/// process never exits with the driver half way through starting.
struct Started<T: Send + 'static>(Option<std::thread::JoinHandle<T>>);

impl<T: Send + 'static> Started<T> {
    fn spawn(job: impl FnOnce() -> T + Send + 'static) -> Self {
        Self(Some(std::thread::spawn(job)))
    }

    /// Wait for the job and take its result; `None` when it panicked.
    fn finish(mut self) -> Option<T> {
        self.0.take().and_then(|h| h.join().ok())
    }
}

impl<T: Send + 'static> Drop for Started<T> {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = h.join();
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one sequence: open, plan, stack, write, report"
)]
fn stack(a: &StackArgs) -> Result<()> {
    let started = Instant::now();
    helia_core::io::ImageFormat::from_path(&a.output)?;
    // Checked now, so a plugin that is off stops the run before minutes of stacking.
    let plugins = PluginRun::new(&a.plugins, true, a.quiet)?;
    if plugins.as_ref().is_some_and(|p| p.backend().is_some()) && a.report.is_some() {
        bail!(
            "--report needs the per-frame results that only a stack made on this computer has; \
             leave out --report or --stack-backend"
        );
    }
    let bar = if a.quiet {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(1000)
    };
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner} {msg:<28} [{bar:40}] {percent:>3}%  {elapsed_precise}",
        )
        .map_err(|e| anyhow::anyhow!("progress style: {e}"))?
        .progress_chars("=> "),
    );
    // The alignment stages interleave; each is announced once.
    let announced = std::cell::RefCell::new(Vec::<Stage>::new());
    // Compressed video is decoded once into a cache file; show that as a stage.
    let decode_bar = bar.clone();
    let video = VideoOptions {
        cache_dir: a.cache_dir.clone(),
        progress: Some(std::sync::Arc::new(move |done, total| {
            decode_bar.set_message("decoding video (ffmpeg)");
            decode_bar.set_position(done * 1000 / total.max(1));
        })),
        ..VideoOptions::default()
    };
    // ffmpeg extracts the colour plane while decoding, so the channel is needed before the
    // capture is open; a video is never raw colour, so the preset cannot change it.
    let mut video_note = None;
    let video_channel = match a.channel {
        Some(c) => match c {
            ChannelArg::R => Channel::Red,
            ChannelArg::G => Channel::Green,
            ChannelArg::B => Channel::Blue,
            ChannelArg::Lum => Channel::Luminance,
        },
        // Nothing named: the luminance, unless a plane of the video is clipped.
        None if ffmpeg::is_video_path(&a.input) => match ffmpeg::choose_channel(&a.input) {
            Ok(choice) => {
                video_note = choice.note;
                choice.channel
            }
            Err(_) => Channel::Luminance,
        },
        None => Channel::Luminance,
    };
    // Starting the device takes a few tenths of a second; do it while the
    // capture is opened and probed.
    let choice = a.backend;
    let backend_thread = Started::spawn(move || select_backend(choice));
    let open_options = helia_core::io::source::OpenOptions {
        channel: video_channel,
        ..a.open.options()
    };
    let open = || {
        open_reported_with(&a.input, &open_options, &video)
            .with_context(|| format!("cannot open {}", a.input.display()))
    };
    let (mut src, capture) = open()?;
    let (width, height) = src.dimensions();
    let is_bayer = src.read(0).is_ok_and(|f| f.bayer.is_some());
    let rec = recommend(width, height, is_bayer, &capture_texts(&a.input), a.preset);
    let mut cfg = build_config(a, &rec)?;
    let calibration = resolve_calibration(a, &mut cfg, width, height)?;
    let colour = colour_params(a, &rec)?;
    // Built (and so checked) before the minutes of stacking, not after.
    let edits = a.edits.options()?.build(rec.preset, width, height)?;
    save_edit_preset(&a.edits, &edits, rec.preset)?;
    let plan = OutputPlan {
        edits,
        colour,
        edit_fits: a.sharpen_fits,
        lead_notes: Vec::new(),
        fits: None,
        rgb: None,
    };
    let backend = backend_thread
        .finish()
        .ok_or_else(|| anyhow::anyhow!("backend start-up panicked"))??;
    if !a.quiet {
        eprintln!("input:    {}", src.describe());
        if let Some(note) = src.quality_note() {
            eprintln!("note:     {note}");
        }
        for line in notice_lines(&capture) {
            eprintln!("{line}");
        }
        eprintln!("backend:  {}", backend.name());
        eprintln!("settings: {}", recommendation_used(a, &rec).summary());
        if let Some(note) = &video_note {
            eprintln!("channel: {note}");
        }
        for l in &calibration {
            eprintln!("{l}");
        }
        if !plan.edits.is_empty() {
            for l in plan.edits.describe() {
                eprintln!("edit:     {l}");
            }
        }
        if let Some(p) = rec.preset
            && a.preset.is_none()
            && a.colour.is_none()
        {
            eprintln!("tip:      add --colour auto for {} colours", p.name());
        }
    }
    let progress = |stage: Stage, f: f32| {
        if !a.quiet && !announced.borrow().contains(&stage) {
            announced.borrow_mut().push(stage);
            stage_line(&bar, started, stage.label());
        }
        bar.set_message(stage.label());
        bar.set_position((f * 1000.0) as u64);
    };
    let opened = started.elapsed();
    let mut by_plugin = false;
    let mut out = if let Some(p) = plugins.as_ref().filter(|p| p.backend().is_some()) {
        let name = p.backend().unwrap_or_default().to_owned();
        let job = backend_job(
            a,
            &cfg,
            &calibration,
            rec.preset.map(helia_core::Preset::id),
        )?;
        bar.set_message(format!("stacking with plugin {name}"));
        let stacked = p.stack(&a.input, &job)?;
        by_plugin = true;
        if !a.quiet
            && let Some(note) = &stacked.note
        {
            eprintln!("plugin:   {name}: {note}");
        }
        helia_core::pipeline::StackOutput::from_image(stacked.frame, format!("plugin {name}"))
    } else {
        // Frame steps of plugins see each frame on its way into the pipeline; the summary
        // below reads the capture again without them.
        let (mut piped, failure) = match plugins.as_ref().filter(|p| p.has_frame_steps()) {
            Some(p) => p.wrap(open()?.0),
            None => (open()?.0, Arc::default()),
        };
        let run = Pipeline::new(cfg.clone())
            .with_backend(Arc::clone(&backend))
            .run(piped.as_mut(), &progress, &AtomicBool::new(false));
        match run {
            Ok(o) => o,
            Err(e) => {
                let own = failure.lock().ok().and_then(|mut f| f.take());
                bar.finish_and_clear();
                return Err(match own {
                    Some(h) => anyhow::anyhow!("{h}"),
                    None => anyhow::Error::new(e).context("stacking failed"),
                });
            }
        }
    };
    bar.finish_and_clear();
    if let Some(p) = plugins.as_ref().filter(|p| p.has_image(StepStage::PreEdit)) {
        out.image = p.image(StepStage::PreEdit, out.image.clone())?;
    }

    let granulation = helia_core::granulation::for_stack(
        &out.image,
        rec.preset,
        out.disc.map(|d| d.r),
        a.aperture_mm,
    );
    let spot_detection = if a.sunspots {
        stack_sunspots(a, &out, &CaptureInfo::read(&a.input, rec.preset), a.quiet)
    } else {
        None
    };
    let write_started = Instant::now();
    let prepared = write_result(
        a,
        &out,
        rec.preset,
        granulation.as_ref(),
        spot_detection.as_ref().map(|d| &d.report),
        &plan,
        &capture,
        &backend,
        started,
        plugins.as_ref(),
    )?;
    let written = write_started.elapsed();
    if a.timings && !by_plugin {
        print_timings(&out, opened, written, started.elapsed());
    }
    if !a.quiet && by_plugin {
        eprintln!(
            "stacked:  by plugin {} in {:.1}s",
            plugins
                .as_ref()
                .and_then(PluginRun::backend)
                .unwrap_or_default(),
            started.elapsed().as_secs_f32()
        );
        eprintln!("wrote:   {}", a.output.display());
    } else if !a.quiet {
        print_summary(
            &out,
            &mut src,
            &cfg,
            prepared.edited.then_some(&prepared.image),
            &plan.edits,
            a.edits.flatten.is_some(),
            started.elapsed().as_secs_f32(),
        )?;
        if let Some(g) = &granulation {
            for l in g.lines() {
                eprintln!("granulation: {l}");
            }
        }
        if let Some(s) = &out.seeing {
            for l in s.lines() {
                eprintln!("seeing:  {l}");
            }
        }
        if let Some(p) = &out.field_rotation {
            for l in p.lines(out.disc.map(|d| d.r)) {
                eprintln!("rotation: {l}");
            }
        }
        if let Some(note) = &out.field_rotation_note {
            eprintln!("rotation: {note}");
        }
        eprintln!("wrote:   {}", a.output.display());
    }
    if let Some(w) = non_solar_warning(out.disc.is_some(), rec.preset.is_some()) {
        eprintln!("warning: {w}");
    }
    if let Some(p) = &plugins {
        p.analyse(&prepared.image, &analysis_path(&a.output))?;
        p.close();
    }
    if let Some(dir) = &a.report {
        let info = CaptureInfo::read(&a.input, rec.preset);
        let when = a.date.or_else(|| {
            FitsMeta::from_stack(&info, &out, None)
                .date_avg
                .or_else(|| info.mid_unix())
        });
        let tuning = a.sunspot.params();
        let solar = helia_report::SolarInputs {
            when_unix: when,
            celestial_north_up: tuning.celestial_north_up,
            rotate_deg: tuning.rotate_deg,
            mirror: tuning.mirror,
            sunspots: spot_detection
                .as_ref()
                .map(|d| (d.report.clone(), d.overlay.clone())),
        };
        write_report(
            a,
            &out,
            &cfg,
            &mut src,
            prepared.edited.then_some(&prepared.image),
            solar,
            dir,
        )?;
    }
    Ok(())
}

/// `--report DIR`: the science outputs of the stack.
fn write_report(
    a: &StackArgs,
    out: &helia_core::pipeline::StackOutput,
    cfg: &StackConfig,
    src: &mut dyn FrameSource,
    sharpened: Option<&helia_core::Frame>,
    solar: helia_report::SolarInputs,
    dir: &Path,
) -> Result<()> {
    let metric = format!("{:?}", cfg.metric);
    let mut extras = helia_report::Extras::gather(src, cfg.debayer, out);
    extras.sharpened = sharpened.cloned();
    extras.solar = solar;
    extras.aperture_mm = a.aperture_mm;
    extras.wavelength_nm = cfg.seeing.and_then(|s| s.wavelength_nm);
    let data = helia_report::ScienceData::from_output(out, &metric, &extras);
    let name = |p: &Path| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let meta = helia_report::html::Meta {
        title: name(&a.input),
        inputs: vec![
            ("capture".into(), a.input.display().to_string()),
            ("stack".into(), a.output.display().to_string()),
        ],
        settings: helia_report::settings::from_config(cfg, out),
        generated: helia_core::io::solarnet::iso_from_unix(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
        ),
        version: helia_core::about::display_version(env!("CARGO_PKG_VERSION")),
    };
    let fits = helia_report::settings::fits_header_summary(&a.output);
    let bundle = helia_report::generate(&data, &meta, fits, helia_report::canvas::Size::default())
        .context("cannot make the report")?;
    bundle
        .write(dir)
        .with_context(|| format!("cannot write the report to {}", dir.display()))?;
    if !a.quiet {
        for s in &bundle.sections {
            for n in &s.notes {
                eprintln!("report:  not drawn: {n}");
            }
        }
        eprintln!(
            "report:  {} files in {} (open {})",
            bundle.files.len(),
            dir.display(),
            dir.join(helia_report::REPORT_HTML).display()
        );
    }
    Ok(())
}

/// `helia plot KIND INPUT`: one figure, from a report directory or a fresh stack.
fn plot(a: &helia_cli::PlotArgs) -> Result<()> {
    use helia_cli::PlotChoice;
    let size = helia_report::canvas::Size {
        width: a.size.0,
        height: a.size.1,
    };
    let fresh = !a.input.is_dir();
    let scratch = std::env::temp_dir().join(format!("helia-plot-{}", std::process::id()));
    let dir: &Path = if fresh { &scratch } else { &a.input };
    if fresh {
        let mut argv: Vec<std::ffi::OsString> =
            vec!["helia".into(), "stack".into(), a.input.clone().into()];
        argv.extend([
            "-o".into(),
            scratch.join("stack.tif").into(),
            "--report".into(),
            scratch.clone().into(),
        ]);
        argv.extend(a.stack_options.iter().map(Into::into));
        std::fs::create_dir_all(&scratch)?;
        let cli = Cli::try_parse_from(argv).map_err(|e| anyhow::anyhow!("{e}"))?;
        let Command::Stack(sa) = cli.command else {
            bail!("the options after -- must be options of `helia stack`");
        };
        if let Err(e) = stack(&sa) {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(e);
        }
    }
    let result = (|| -> Result<()> {
        let data = helia_report::ScienceData::load(dir)?;
        match a.kind {
            PlotChoice::One(k) => {
                let png = data.plot(k, size)?;
                let path = a
                    .output
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(format!("{}.png", k.name())));
                std::fs::write(&path, png)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                eprintln!("wrote:   {}", path.display());
            }
            PlotChoice::All => {
                let out_dir = a.output.clone().unwrap_or_else(|| PathBuf::from("."));
                std::fs::create_dir_all(&out_dir)?;
                for k in helia_report::PlotKind::ALL {
                    match data.plot(k, size) {
                        Ok(png) => {
                            let path = out_dir.join(format!("{}.png", k.name()));
                            std::fs::write(&path, png)?;
                            eprintln!("wrote:   {}", path.display());
                        }
                        Err(helia_report::Error::NoData(why)) => {
                            eprintln!("skipped: {}: {why}", k.name());
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
        Ok(())
    })();
    if fresh {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    result
}

/// Build or read the masters `--dark`, `--flat` and `--bad-pixels` name, put
/// them in `cfg` and return the lines to print about them.
fn resolve_calibration(
    a: &StackArgs,
    cfg: &mut StackConfig,
    width: usize,
    height: usize,
) -> Result<Vec<String>> {
    if !a.calibration.any() {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let exposure = CaptureInfo::read(&a.input, None).exposure_s;
    let spec = a.calibration.spec(a.open.options());
    let last = std::cell::RefCell::new(String::new());
    let progress = |what: &str, _f: f32| {
        if !a.quiet && *last.borrow() != what {
            what.clone_into(&mut last.borrow_mut());
            eprintln!("[{:>6.1}s] {what}", started.elapsed().as_secs_f32());
        }
    };
    let resolved = spec
        .resolve(exposure, &progress, &AtomicBool::new(false))
        .context("cannot prepare the calibration")?;
    let cal = resolved.calibration;
    // The sizes are checked now, not after the capture has been scored. The
    // mosaic pattern is checked against the real frames when the stack runs.
    cal.plan(width, height, cal.dark.as_ref().and_then(|m| m.info.bayer))
        .context("the calibration does not fit this capture")?;
    let mut lines: Vec<String> = cal
        .summary_lines()
        .into_iter()
        .map(|l| format!("calibrate: {l}"))
        .collect();
    lines.extend(resolved.warnings.iter().map(|w| format!("warning:   {w}")));
    cfg.calibration = Some(cal.shared());
    Ok(lines)
}

/// Sharpen and edit the stack, colour it if asked, and write it where
/// `--output` says. Sharpening comes first and colour after. FITS is the
/// unedited stack unless `--sharpen-fits` was given, and stays mono: the
/// colour choice is recorded as a header note.
#[allow(
    clippy::too_many_arguments,
    reason = "everything the file is made from; a struct would only rename the arguments"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: notes, edits, plugins, colour, write"
)]
fn write_result(
    a: &StackArgs,
    out: &helia_core::pipeline::StackOutput,
    filter: Option<Preset>,
    granulation: Option<&helia_core::granulation::Granulation>,
    sunspots: Option<&helia_core::sunspots::Report>,
    plan: &OutputPlan,
    capture: &CaptureReport,
    backend: &Arc<dyn helia_core::Backend>,
    started: Instant,
    plugins: Option<&PluginRun>,
) -> Result<Prepared> {
    let image = &out.image;
    let format = ImageFormat::from_path(&a.output)?;
    let mut plan = plan.clone();
    plan.lead_notes.extend(capture.note_lines());
    plan.lead_notes
        .extend(out.calibration_notes.iter().cloned());
    plan.lead_notes
        .extend(out.channel_note.iter().map(|n| format!("Channel: {n}")));
    if let Some(c) = &out.colour {
        plan.rgb = Some(c.rgb.clone());
        plan.lead_notes.extend(c.notes.iter().cloned());
    }
    let plan = &plan;
    let ctx = || format!("cannot write {}", a.output.display());
    let line = |text: &str| {
        if !a.quiet {
            stage_line(&ProgressBar::hidden(), started, text);
        }
    };
    if !a.quiet && stack_mode(a) != StackMode::Mean {
        eprintln!("note:     {}", helia_core::stack::ROBUST_SCOPE_NOTE);
    }
    let fits = format == ImageFormat::Fits;
    if fits && !a.quiet && out.colour.is_some() {
        eprintln!("note:     a true-colour FITS file is a three-plane cube (red, green, blue)");
    }
    if fits && !a.quiet {
        if plan.colour.is_colour() {
            eprintln!("note:     FITS stays mono; the colour is recorded in its header only");
        }
        if !plan.edits.is_empty() {
            if plan.edit_fits {
                eprintln!(
                    "note:     the sharpening and edits are applied to the FITS file (--sharpen-fits)"
                );
            } else {
                eprintln!(
                    "note:     FITS is the unsharpened stack; the sharpening and edits go to TIFF and PNG (add --sharpen-fits to change that)"
                );
            }
        }
    }
    if !plan.edits.is_empty() && (!fits || plan.edit_fits) {
        line("sharpening and editing");
    }
    let with_meta;
    let plan = if fits {
        let site = a.site.or_else(Site::from_env);
        if site.is_none() && !a.quiet {
            eprintln!("note:     no --site given; OBSGEO-X/Y/Z are not written to the FITS header");
        }
        with_meta = OutputPlan {
            fits: Some(
                FitsMeta::from_stack(&CaptureInfo::read(&a.input, filter), out, site)
                    .with_granulation(granulation.cloned())
                    .with_sunspots(sunspots.cloned())
                    .with_seeing(out.seeing.clone()),
            ),
            ..plan.clone()
        };
        &with_meta
    } else {
        plan
    };
    let mut prepared = prepare(backend.as_ref(), image, &a.output, plan).with_context(ctx)?;
    if let Some(p) = plugins.filter(|p| p.has_image(StepStage::PostEdit)) {
        // Like Helia's own edits, these skip FITS unless --sharpen-fits asks for them.
        if fits && !plan.edit_fits {
            if !a.quiet {
                eprintln!(
                    "note:     the plugin steps after the edits are not applied to FITS (use pre:PLUGIN/STEP, or --sharpen-fits)"
                );
            }
        } else if prepared.rgb.is_some() {
            bail!(
                "A true-colour stack cannot go through a plugin image step. Those steps work on one-channel pictures, and this one has three channels. What to do: stack with --colour mono, or leave out the image step."
            );
        } else {
            line("plugin steps");
            prepared.image = p.image(StepStage::PostEdit, prepared.image.clone())?;
            prepared
                .lines
                .push(p.describe_image_steps(StepStage::PostEdit));
        }
    }
    if let Some(p) = plugins {
        prepared.lines.extend(p.provenance());
    }
    if plan.colour.preset.is_some() && !fits {
        line("colourising");
    }
    line("writing");
    // The plugins that took part are named in the FITS header too (only they could say so now).
    let with_cards;
    let plan = match (plugins, &plan.fits) {
        (Some(p), Some(f)) => {
            with_cards = OutputPlan {
                fits: Some(f.clone().with_plugin_cards(p.fits_cards())),
                ..plan.clone()
            };
            &with_cards
        }
        _ => plan,
    };
    finish(backend.as_ref(), &prepared, &a.output, plan).with_context(ctx)?;
    Ok(prepared)
}

/// Save the edit list as a named preset when `--save-edits` asked.
fn save_edit_preset(
    args: &helia_cli::EditArgs,
    edits: &helia_core::EditList,
    preset: Option<Preset>,
) -> Result<()> {
    let Some(name) = &args.save_edits else {
        return Ok(());
    };
    let dir = store::presets_dir()
        .context("no place to keep edit presets: set HELIA_EDIT_PRESETS to a folder")?;
    let path = store::save(
        &dir,
        &store::EditPreset {
            name: name.clone(),
            filter: preset.map(|p| p.id().to_string()),
            edits: edits.clone(),
        },
    )?;
    eprintln!("preset:   saved {} as {}", name, path.display());
    Ok(())
}

/// `helia edit`: sharpen and adjust an image that already exists. With no edit
/// options it converts the image to the output's format and changes nothing else.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: read, plan, plugins, write"
)]
fn edit(a: &EditCommandArgs) -> Result<()> {
    let started = Instant::now();
    ImageFormat::from_path(&a.output)?;
    let plugins = PluginRun::new(&a.plugins, false, a.quiet)?;
    let (mut image, description) = helia_core::io::any::read_any(&a.input)
        .with_context(|| format!("cannot read {}", a.input.display()))?;
    let (width, height) = image.size();
    let rec = recommend(width, height, false, &capture_texts(&a.input), a.preset);
    let edits = a.edits.options()?.build(rec.preset, width, height)?;
    // No edit of any kind: convert the image to the output's format, unchanged.
    let convert_only = edits.is_empty() && a.edits.save_edits.is_none() && plugins.is_none();
    save_edit_preset(&a.edits, &edits, rec.preset)?;
    if edits.is_empty() && plugins.is_none() && !convert_only {
        return Ok(());
    }
    if let Some(p) = plugins.as_ref().filter(|p| p.has_image(StepStage::PreEdit)) {
        let helia_core::io::any::Loaded::Mono(f) = &image else {
            bail!(
                "This picture is in colour, and plugin image steps work on one-channel pictures. What to do: run the step on the mono image before it is coloured, with helia stack --colour mono."
            );
        };
        image = helia_core::io::any::Loaded::Mono(p.image(StepStage::PreEdit, f.clone())?);
    }
    let backend = select_backend(a.backend)?;
    if !a.quiet {
        eprintln!("input:    {} ({width}x{height})", a.input.display());
        eprintln!("backend:  {}", backend.name());
        eprintln!("settings: {}", rec.summary());
        for l in edits.describe() {
            eprintln!("edit:     {l}");
        }
        if convert_only {
            eprintln!(
                "edit:     none given, so the image is only converted to the format of the output name"
            );
        }
    }
    let mut lead = vec![format!(
        "Edited from {}",
        a.input.file_name().map_or_else(
            || "an image".to_string(),
            |n| n.to_string_lossy().into_owned()
        )
    )];
    if let Some(d) = description.filter(|d| !d.is_empty()) {
        lead.push(format!("Earlier history: {d}"));
    }
    let post = plugins
        .as_ref()
        .filter(|p| p.has_image(StepStage::PostEdit) || p.has_analyse());
    let written = if let (Some(p), helia_core::io::any::Loaded::Mono(f)) = (post, &image) {
        // Edits, then the plugin's steps, then the file: the order `helia stack` uses.
        let plan = OutputPlan {
            edits: edits.clone(),
            edit_fits: true,
            lead_notes: lead.clone(),
            ..OutputPlan::default()
        };
        let mut prepared = prepare(backend.as_ref(), f, &a.output, &plan)
            .with_context(|| format!("cannot write {}", a.output.display()))?;
        if p.has_image(StepStage::PostEdit) {
            prepared.image = p.image(StepStage::PostEdit, prepared.image.clone())?;
            prepared
                .lines
                .push(p.describe_image_steps(StepStage::PostEdit));
        }
        prepared.lines.extend(p.provenance());
        let w = finish(backend.as_ref(), &prepared, &a.output, &plan)
            .with_context(|| format!("cannot write {}", a.output.display()))?;
        p.analyse(&prepared.image, &analysis_path(&a.output))?;
        w
    } else {
        if post.is_some() {
            bail!(
                "This picture is in colour, and plugin image steps work on one-channel pictures. What to do: run the step on the mono image before it is coloured, with helia stack --colour mono."
            );
        }
        if let Some(p) = &plugins {
            lead.extend(p.provenance());
        }
        write_loaded(backend.as_ref(), &image, &a.output, &edits, &lead)
            .with_context(|| format!("cannot write {}", a.output.display()))?
    };
    if let Some(p) = &plugins {
        p.close();
    }
    if !a.quiet
        && let helia_core::io::any::Loaded::Mono(f) = &image
        && let Ok(after) = backend.apply_edits(f, &edits)
        && after.width == f.width
        && after.height == f.height
    {
        report_edit_effect(f, &after, &edits);
    }
    if !a.quiet {
        eprintln!(
            "wrote:    {} ({}x{}) in {:.2}s",
            a.output.display(),
            written.width,
            written.height,
            started.elapsed().as_secs_f32()
        );
    }
    Ok(())
}

/// The plain-English warning for a capture that does not look like the Sun:
/// no solar disc was found and no filter preset was named or recognised. The
/// stack is made anyway, on the settings for the Sun, which may not suit it.
fn non_solar_warning(disc_found: bool, preset_known: bool) -> Option<String> {
    (!disc_found && !preset_known).then(|| {
        "this capture does not look like the Sun: no solar disc was found and no filter was recognised. Helia's alignment, sharpening and levels are tuned for the Sun, so a planet, the Moon or a star field is stacked on settings that may clip bright detail or leave it soft (the wavelet sharpening of a small bright object can saturate). The stack was made anyway. What to do: check the stack on its own first (leave --sharpen off), sharpen gently with --sharpen unsharp, or lower the wavelet gains with --wavelet-gains; for a full disc of the Sun, name the filter with --preset.".to_string()
    })
}

/// Frame counts, alignment points, the sharpness comparison and timings.
fn print_summary(
    out: &helia_core::pipeline::StackOutput,
    src: &mut dyn FrameSource,
    cfg: &StackConfig,
    edited: Option<&helia_core::Frame>,
    edits: &helia_core::EditList,
    a_flatten: bool,
    total_seconds: f32,
) -> Result<()> {
    // Report detail on a lightly smoothed image so pixel noise (which
    // stacking removes) does not count as sharpness.
    let detail = |f: &helia_core::Frame| {
        quality::score(&filter::gaussian_blur(f, 2.0), Metric::GradientEnergy, None)
    };
    let mut best = cfg.debayer.apply_owned(src.read(out.ranking[0])?);
    // Compare like with like: the same region as the cropped stack.
    if let Some(c) = &out.crop {
        best = helia_core::autocrop::cut(&best, c.rect);
    }
    eprintln!(
        "frames:  {} total, {} kept, {} registered",
        out.scores.len(),
        out.kept.len(),
        out.shifts.len()
    );
    if !out.score_outliers.is_empty() {
        let list: Vec<String> = out
            .score_outliers
            .iter()
            .map(|(i, _)| i.to_string())
            .collect();
        eprintln!(
            "ranking: {} frame(s) scored high by an artefact were ranked at their neighbours' level (frames {}, counting from 0)",
            list.len(),
            list.join(", ")
        );
    }
    if let Some(g) = &out.grid {
        eprintln!(
            "points:  {} of {} alignment points active",
            g.active_count(),
            g.aps.len()
        );
    }
    if let Some(c) = &out.auto_ap {
        eprintln!("auto-ap: {}", c.summary());
    }
    if let Some(r) = &out.refine {
        eprintln!("refine:  {}", r.summary());
    }
    if let Some(c) = &out.colour {
        let [r, g, b] = c.gains;
        eprintln!("colour:  true colour, gains red {r:.3}, green {g:.3}, blue {b:.3}");
        for line in c.notes.iter().skip(1) {
            eprintln!("         {line}");
        }
    }
    if let Some(note) = &out.channel_note {
        eprintln!("channel: {note}");
    }
    eprintln!("align:   {}", out.alignment.summary());
    match (&out.limb, a_flatten) {
        (Some(l), _) => eprintln!("limb:    {}", l.summary()),
        (None, true) => eprintln!("limb:    no clear disc to fit, so --flatten changed nothing"),
        (None, false) => {}
    }
    if let Some(c) = &out.crop {
        eprintln!(
            "crop:    {}x{} from ({}, {}), disc plus {:.0}%",
            c.rect.width(),
            c.rect.height(),
            c.rect.x0,
            c.rect.y0,
            c.margin * 100.0
        );
    } else if let Some(why) = out.crop_skipped {
        eprintln!("crop:    {}", why.reason());
    }
    for r in &out.masked {
        eprintln!(
            "masked:  {} x {} at ({}, {}), filled from the sky around it",
            r.width(),
            r.height(),
            r.x0,
            r.y0
        );
    }
    if let Some(d) = &out.disc {
        eprintln!(
            "disc:    centre ({:.1}, {:.1}), radius {:.1} px",
            d.cx, d.cy, d.r
        );
    }
    let (stacked, single) = (detail(&out.image), detail(&best));
    eprintln!(
        "detail:  stack {stacked:.5} vs best single frame {single:.5} ({:.2}x)",
        stacked / single.max(1e-12)
    );
    if let Some(w) = blurred_stack_warning(stacked / single.max(1e-12)) {
        eprintln!("warning: {w}");
    }
    if let Some(e) = edited {
        report_edit_effect(&out.image, e, edits);
    }
    let stages: Vec<String> = out
        .timings
        .iter()
        .map(|(st, d)| format!("{} {:.2}s", st.label(), d.as_secs_f32()))
        .collect();
    eprintln!("timing:  {} (total {total_seconds:.2}s)", stages.join(", "));
    Ok(())
}

/// Below this ratio of the stack's detail to the best single frame's, the
/// stack is called blurrier than its own input.
const BLURRED_STACK_RATIO: f32 = 0.75;

/// The plain-English warning for a stack that is less sharp than the best frame
/// that went into it, or `None` when it is not. Adding frames that do not line
/// up smears them, so the usual causes are named with what to do about each.
fn blurred_stack_warning(ratio: f32) -> Option<String> {
    (ratio < BLURRED_STACK_RATIO).then(|| {
        format!(
            "the stack is less sharp than the best single frame it was made from ({ratio:.2}x). Frames that do not line up smear each other when they are added; the usual causes are field rotation (an altazimuth mount turns the picture over a long capture) and frames that could not be matched. What to do: for FITS subs, check the rotation line above (the correction is automatic when the headers name the target, site and mount, and --field-rotation asks for it otherwise); look at the align line for a low confidence; or keep fewer frames with --keep-frames."
        )
    })
}

/// How much finer detail an edit list gave a stack, and what it did to the
/// noise. Detail is the existing sharpness metric (gradient energy of the image
/// smoothed by 2 px); the noise is the median-based sigma of the finest wavelet
/// layer, which is mostly noise.
fn report_edit_effect(
    before: &helia_core::Frame,
    after: &helia_core::Frame,
    edits: &helia_core::EditList,
) {
    let detail = |f: &helia_core::Frame| {
        quality::score(&filter::gaussian_blur(f, 2.0), Metric::GradientEnergy, None)
    };
    let fine = |f: &helia_core::Frame| {
        let smooth = helia_core::edit::wavelet::smooth(&f.data, f.width, f.height, 1);
        helia_core::edit::wavelet::mad_sigma_of_difference(&f.data, &smooth, f.width, f.height)
    };
    let (d0, d1) = (detail(before), detail(after));
    eprintln!(
        "sharpened: detail {d1:.5} after the edits, {d0:.5} before ({:.2}x)",
        d1 / d0.max(1e-12)
    );
    eprintln!(
        "noise:   finest-layer sigma {:.5} before, {:.5} after the edits",
        fine(before),
        fine(after)
    );
    let psf = edits.resolve(before).ops.iter().find_map(|op| match op {
        helia_core::Op::Deconvolve(d) => Some(d.psf),
        _ => None,
    });
    if let Some(p) = psf {
        eprintln!("psf:     {} (resolved on this image)", p.describe());
    }
    if let Some(c) = helia_core::edit::diagnose::check(before, after, psf) {
        eprintln!("check:   {}", c.summary());
        if let Some(w) = c.warning() {
            eprintln!("warning: {w}");
        }
    }
}

/// Wall time per stage and busy time per kind of work.
fn print_timings(
    out: &helia_core::pipeline::StackOutput,
    opened: std::time::Duration,
    written: std::time::Duration,
    total: std::time::Duration,
) {
    let threads = helia_core::profile::threads() as f32;
    eprintln!("timings: wall seconds per stage");
    eprintln!("  {:<28} {:>7.2}", "open", opened.as_secs_f32());
    for (st, d) in &out.timings {
        eprintln!("  {:<28} {:>7.2}", st.label(), d.as_secs_f32());
    }
    eprintln!("  {:<28} {:>7.2}", "write", written.as_secs_f32());
    eprintln!("  {:<28} {:>7.2}", "total", total.as_secs_f32());
    eprintln!("timings: busy seconds summed over threads (and per thread of {threads})");
    for (cat, d) in &out.busy {
        eprintln!(
            "  {:<28} {:>7.2} {:>7.2}",
            cat.label(),
            d.as_secs_f32(),
            d.as_secs_f32() / threads
        );
    }
}

/// `helia calibrate`: masters and a bad-pixel map from calibration captures.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence: darks, bias, flats, bad pixels, report"
)]
fn calibrate(a: &CalibrateArgs) -> Result<()> {
    use helia_core::calibrate::badpix::{self, DetectOptions};
    use helia_core::calibrate::diag;
    use helia_core::calibrate::master::{
        BuildOptions, CalInput, Master, MasterKind, build_dark, build_flat,
    };
    use helia_core::calibrate::selfflat::{SelfFlatOptions, Smooth, build_selfflat};

    if a.darks.is_empty()
        && a.flats.is_empty()
        && a.displaced.is_empty()
        && a.bias.is_empty()
        && a.lights.is_none()
    {
        bail!("nothing to do: give --darks, --flats, --displaced, --bias or --lights");
    }
    std::fs::create_dir_all(&a.output_dir)
        .with_context(|| format!("cannot create {}", a.output_dir.display()))?;
    let started = Instant::now();
    let say = |text: String| {
        if !a.quiet {
            eprintln!("[{:>6.1}s] {text}", started.elapsed().as_secs_f32());
        }
    };
    let opts = BuildOptions {
        combine: a.combine,
        max_frames: a.max_frames.unwrap_or(0),
        ..BuildOptions::default()
    };
    let open = a.open.options();
    let cancel = AtomicBool::new(false);
    let none = |_: f32| {};
    let mut warnings: Vec<String> = Vec::new();
    let mut written: Vec<PathBuf> = Vec::new();
    let mut make = |paths: &[PathBuf], kind: MasterKind, file: &str| -> Result<Option<Master>> {
        if paths.is_empty() {
            return Ok(None);
        }
        say(format!(
            "reading the {} input(s) for the master {}",
            paths.len(),
            kind.name()
        ));
        let mut input = CalInput::open(paths, &open)?;
        warnings.extend(input.notes.iter().cloned());
        say(format!(
            "combining {} frame(s) of {}x{} ({})",
            input.frame_count(),
            input.size().0,
            input.size().1,
            a.combine.label()
        ));
        let m = build_dark(kind, &mut input, &opts, &none, &cancel)?;
        let path = a.output_dir.join(file);
        m.write(&path)?;
        println!("wrote:    {}  ({})", path.display(), m.describe());
        written.push(path);
        Ok(Some(m))
    };
    let dark = make(&a.darks, MasterKind::Dark, "master-dark.fits")?;
    let bias = make(&a.bias, MasterKind::Bias, "master-bias.fits")?;
    let flat_dark = make(&a.flat_darks, MasterKind::Dark, "master-flat-dark.fits")?;

    let mut flat = None;
    if !a.flats.is_empty() {
        say(format!("reading the {} flat input(s)", a.flats.len()));
        let mut input = CalInput::open(&a.flats, &open)?;
        warnings.extend(input.notes.iter().cloned());
        let base = flat_dark.as_ref().or(bias.as_ref());
        let built = build_flat(&mut input, base, &opts, &none, &cancel)?;
        let path = a.output_dir.join("master-flat.fits");
        built.master.write(&path)?;
        println!(
            "wrote:    {}  ({})",
            path.display(),
            built.master.describe()
        );
        println!(
            "flat:     edge of the field {:.0}% of the centre; rings {:.2}% ({} crossings); dust {:.1}% of the field",
            built.diagnostics.edge_to_centre * 100.0,
            built.diagnostics.ring_amplitude * 100.0,
            built.diagnostics.ring_crossings,
            built.diagnostics.dust_fraction * 100.0
        );
        warnings.extend(built.warnings);
        warnings.extend(diag::filter_advice(a.preset));
        flat = Some(built.master);
        written.push(path);
    }

    if !a.displaced.is_empty() {
        say(format!(
            "reading the {} displaced-disc input(s)",
            a.displaced.len()
        ));
        let mut input = CalInput::open(&a.displaced, &open)?;
        warnings.extend(input.notes.iter().cloned());
        say(format!(
            "solving the flat from {} frame(s) of {}x{}",
            input.frame_count(),
            input.size().0,
            input.size().1
        ));
        let few = BuildOptions {
            max_frames: a
                .max_frames
                .unwrap_or(helia_core::calibrate::selfflat::DEFAULT_FRAMES),
            ..opts
        };
        let built = build_selfflat(
            &mut input,
            dark.as_ref(),
            &few,
            &SelfFlatOptions {
                smooth: if a.displaced_keep_smooth {
                    Smooth::Keep
                } else {
                    Smooth::Auto
                },
                ..SelfFlatOptions::default()
            },
            &none,
            &cancel,
        )?;
        let path = a.output_dir.join("master-flat.fits");
        built.master.write(&path)?;
        println!(
            "wrote:    {}  ({})",
            path.display(),
            built.master.describe()
        );
        let r = &built.report;
        println!(
            "selfcal:  {} frames, {} plane(s), misfit {:.2}% RMS, {:.0}% of the sensor covered; the Sun moved {:.0} px, so wavelengths over about {:.0} px are not recovered well",
            r.frames,
            r.planes,
            r.residual * 100.0,
            r.coverage * 100.0,
            r.span_px,
            r.smooth_scale_px
        );
        warnings.extend(built.warnings);
        warnings.extend(diag::filter_advice(a.preset));
        flat = Some(built.master);
        written.push(path);
    }

    if !a.no_bad_pixels {
        let det = DetectOptions {
            hot_sigma: a.hot_sigma,
            ..DetectOptions::default()
        };
        let mosaic = dark
            .as_ref()
            .or(flat.as_ref())
            .is_some_and(|m| m.frame.bayer.is_some());
        let mut map = None;
        if dark.is_some() || flat.is_some() {
            say("looking for hot and cold pixels".to_owned());
            map = Some(badpix::detect(
                dark.as_ref().map(|m| &m.frame),
                flat.as_ref().map(|m| &m.frame),
                mosaic,
                &det,
                "the dark and flat",
            ));
        }
        if let Some(lights) = &a.lights {
            say("looking for stuck pixels in the light frames".to_owned());
            let mut src = helia_core::io::source::open_capture(lights, &open)
                .with_context(|| format!("cannot open {}", lights.display()))?;
            let is_mosaic = src.read(0).is_ok_and(|f| f.bayer.is_some());
            let stuck = badpix::find_stuck(src.as_mut(), 200, is_mosaic, &det, "the lights")?;
            match &mut map {
                Some(existing)
                    if (existing.width, existing.height) == (stuck.width, stuck.height) =>
                {
                    existing.merge(&stuck);
                }
                Some(_) => warnings.push(
                    "the lights are a different size from the darks and flats: their stuck pixels were not merged"
                        .to_owned(),
                ),
                None => map = Some(stuck),
            }
        }
        if let Some(map) = map {
            let path = a.output_dir.join("bad-pixels.fits");
            map.write(&path)?;
            println!("wrote:    {}  ({})", path.display(), map.describe());
            written.push(path);
        }
    }

    for w in &warnings {
        eprintln!("warning:  {w}");
    }
    let q = |p: &Path| {
        let s = p.display().to_string();
        if s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s
        }
    };
    let mut cmd = String::from("helia stack <capture> -o out.tif");
    for item in &written {
        let option = match item.file_name().and_then(|n| n.to_str()) {
            Some("master-dark.fits") => "--dark",
            Some("master-flat.fits") => "--flat",
            Some("master-bias.fits") => "--bias",
            Some("bad-pixels.fits") => "--bad-pixels",
            _ => continue,
        };
        let _ = write!(cmd, " {option} {}", q(item));
    }
    println!("next:     {cmd}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Started;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn a_stack_blurrier_than_its_best_frame_gets_a_plain_warning() {
        let w = super::blurred_stack_warning(0.53).expect("a warning");
        assert!(w.contains("0.53x") && w.contains("What to do:"), "{w}");
        assert!(w.contains("--field-rotation"), "{w}");
        assert!(super::blurred_stack_warning(1.05).is_none());
        assert!(super::blurred_stack_warning(0.8).is_none());
    }

    #[test]
    fn dropping_a_started_job_waits_for_it() {
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let job = Started::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::SeqCst);
        });
        drop(job);
        assert!(done.load(Ordering::SeqCst), "the job was left running");
    }

    #[test]
    fn a_finished_job_gives_its_result_and_a_panic_gives_none() {
        assert_eq!(Started::spawn(|| 7).finish(), Some(7));
        let panicked = Started::spawn(|| -> u8 { panic!("start-up failed") });
        assert_eq!(panicked.finish(), None);
    }
}
