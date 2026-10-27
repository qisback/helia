// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Command-line definition of `helia`.
//!
//! Kept in a library so that `cargo xtask docs` can generate the CLI
//! reference from the same clap definitions the binary parses.

mod edit_args;
pub mod feedback;
pub mod plugins;
pub mod update;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
pub use edit_args::{EditArgs, EditCommandArgs};
use helia_core::align::method::AlignMethod;
use helia_core::io::solarnet::Site;
use helia_core::io::source::{BayerSetting, Endian, OpenOptions};
use helia_core::truecolour::WhiteBalance;
use helia_core::{Error, Keep, Preset};
use helia_gpu::BackendChoice;
pub use plugins::{PluginRunArgs, PluginsAction, PluginsArgs};

/// The `helia` command line.
#[derive(Parser)]
#[command(name = "helia", version, about = "Solar lucky-imaging stacker", long_about = None)]
pub struct Cli {
    /// A plugin to list for this run: a folder holding `plugin.toml`, or the manifest itself.
    /// Repeat for several. A plugin named here is only listed; it runs after you enable it
    /// (`helia plugins enable`). For plugins you are writing or keep outside the plugins folder.
    #[arg(long = "plugin", value_name = "PATH", global = true)]
    pub plugin_paths: Vec<PathBuf>,
    #[command(subcommand)]
    /// The subcommand to run.
    pub command: Command,
}

/// Subcommands of `helia`.
#[derive(Subcommand)]
pub enum Command {
    /// Stack a capture into a single sharp image.
    Stack(Box<StackArgs>),
    /// Build master dark, flat and bad-pixel files from calibration captures, for
    /// `helia stack --dark --flat --bad-pixels`.
    Calibrate(Box<CalibrateArgs>),
    /// Sharpen and adjust an existing TIFF, PNG or FITS image.
    Edit(Box<EditCommandArgs>),
    /// Print what a capture file contains: every header field as written, what
    /// Helia will use instead where it differs, and why.
    Info {
        /// Capture file: .ser or .avi, or a video (.mp4, .mov, .mkv, .webm, .m4v) read
        /// through the optional ffmpeg program, or FITS frames: a .fit, .fits or .fts
        /// file (a cube or several images in one file), a folder of them, or a
        /// pattern such as 'Light_*.fit' (in quotes).
        input: PathBuf,
        #[command(flatten)]
        /// Overrides, to see what they would change.
        open: OpenArgs,
    },
    /// Measure how much granulation an image holds: the band-pass contrast
    /// (standard deviation over mean) of a disc-centre crop in three bands,
    /// for comparing settings on the same capture.
    Measure {
        /// Image: .tif, .tiff, .png or .fits (mono or RGB; RGB is averaged to grey).
        input: PathBuf,
        /// Side of the disc-centre crop in pixels.
        #[arg(long, value_name = "PX", default_value_t = helia_core::measure::DEFAULT_CROP)]
        crop: usize,
        /// Centre the crop on this pixel, as X,Y, instead of on the centroid of
        /// the bright disc. The centroid moves with prominences and sky glow;
        /// give the fitted disc centre from `helia stack` (its `disc:` line) to
        /// measure the same part of the Sun in every stack of a capture.
        #[arg(long, value_name = "X,Y", value_parser = parse_xy)]
        centre: Option<(usize, usize)>,
        /// Instead of the granulation bands, fit the limb-darkening laws (linear,
        /// quadratic, Neckel and Labs) to the disc and print the coefficients and
        /// the measured profile.
        #[arg(long)]
        limb: bool,
        /// Instead of the granulation bands, measure the blur of the solar limb
        /// (the Gaussian sigma `--rl-psf limb` would use) and print it.
        #[arg(long, conflicts_with = "limb")]
        psf: bool,
        /// Image scale in arcseconds per pixel, to give sizes in arcseconds.
        #[arg(long, value_name = "ARCSEC")]
        scale: Option<f32>,
        /// Telescope aperture in millimetres; with --wavelength and --scale
        /// it gives the diffraction cut-off of the power spectrum.
        #[arg(long, value_name = "MM", requires_all = ["wavelength", "scale"])]
        aperture: Option<f32>,
        /// Wavelength in nanometres (for --aperture).
        #[arg(long, value_name = "NM")]
        wavelength: Option<f32>,
    },
    /// Find the sunspots on a stacked white-light image: groups, the relative
    /// Wolf number and areas corrected for foreshortening, as a table, JSON
    /// and an overlay picture.
    Sunspots(SunspotsArgs),
    /// Make a time-lapse: stack each capture with the same settings, register the
    /// stacks to each other (limb fit and phase correlation of the disc centre),
    /// optionally even out the brightness, and write a numbered PNG or TIFF
    /// sequence and a movie (MP4 through ffmpeg, else an animated PNG).
    Timelapse(Box<TimelapseArgs>),
    /// Bring images of the Sun to one time by its differential rotation
    /// (Howard or Snodgrass rates, P, B0 and L0 from the capture times), so that
    /// features line up across a long session or between panels taken minutes
    /// apart.
    #[command(name = "solar-derotate")]
    SolarDerotate(Box<SolarDerotateArgs>),
    /// Turn stacked images of one target to the same orientation, undoing the
    /// field rotation of an altazimuth mount between captures (a time-lapse).
    Derotate(DerotateArgs),
    /// Join panels of the Sun into a full-disc mosaic: match the overlaps (shift and
    /// a few degrees of turn), place all the panels at once by least squares, match
    /// their brightness, join along seams of least difference and feather the
    /// seams. The panels can be derotated to one time first.
    Mosaic(Box<MosaicArgs>),
    /// Draw one science figure again from a report directory that
    /// `helia stack --report` wrote (or stack a capture and draw it).
    Plot(Box<PlotArgs>),
    /// Run the HTTP/JSON API server.
    Serve(ServeArgs),
    /// List the filter presets (`helia stack --preset ...`).
    Presets,
    /// Write a filter's colour table as a file other programs read (.cube,
    /// .csv, `ImageJ` .lut or a PNG strip), so a picture coloured elsewhere
    /// matches one coloured by Helia.
    Lut(LutArgs),
    /// Send feedback (a bug, a feature request or a question). It is sent to
    /// the Helia developers and filed in the Helia issue tracker, through the
    /// feedback service.
    Feedback(FeedbackArgs),
    /// List, inspect, approve, set up and test plugins: programs that add a processing step or
    /// do the stacking elsewhere. Nothing runs until you enable it.
    Plugins(PluginsArgs),
    /// Check for a newer Helia, download it and install it, or go back to the
    /// previous version. Contacts the update server only when you run it.
    Update(update::UpdateArgs),
}

/// Options of `helia derotate`.
#[derive(clap::Args)]
pub struct DerotateArgs {
    /// The images, .fits, .tif or .png, of one session (mono or RGB). Each is
    /// turned to the orientation of the reference image.
    #[arg(required = true, num_args = 1..)]
    pub inputs: Vec<PathBuf>,
    /// Folder to write the turned images to, as NAME-derot.EXT; created if it
    /// does not exist.
    #[arg(short, long, value_name = "DIR")]
    pub output_dir: PathBuf,
    /// Observing site as LAT,LON,ALT (degrees north, degrees east, metres).
    /// Without it the `HELIA_SITE` environment variable is used.
    #[arg(long, value_name = "LAT,LON,ALT", value_parser = clap::value_parser!(Site))]
    pub site: Option<Site>,
    /// When each image was taken, UTC, comma separated, in the order of the
    /// images (2026-10-20T11:30:00,2026-10-20T11:45:00). A FITS file's DATE-AVG
    /// or DATE-OBS is used for an image that this leaves out.
    #[arg(long, value_name = "TIME,...", value_delimiter = ',')]
    pub times: Vec<String>,
    /// The orientation to turn the others to: middle (of the session, the
    /// default), first, last, or the number of an image (from 1).
    #[arg(long, value_name = "WHICH", default_value = "middle")]
    pub reference: String,
    /// The pictures are mirror images (an odd number of reflections, a star
    /// diagonal for one): the turn is the other way.
    #[arg(long)]
    pub mirror: bool,
    /// Overwrite files that exist.
    #[arg(long)]
    pub overwrite: bool,
}

/// Tuning of the sunspot detection (`helia sunspots`, `helia stack --sunspots`).
#[derive(clap::Args, Clone, Debug)]
pub struct SunspotTuning {
    /// Umbra threshold: pixels darker than this share of the quiet disc (after
    /// the limb darkening is flattened out) are umbra.
    #[arg(long, value_name = "SHARE", default_value_t = helia_core::sunspots::Params::default().umbra, help_heading = "Sunspots")]
    pub sunspot_umbra: f32,
    /// Penumbra threshold: pixels darker than this share of the quiet disc are
    /// spot. Raise it to take in fainter penumbrae and pores.
    #[arg(long, value_name = "SHARE", default_value_t = helia_core::sunspots::Params::default().penumbra, help_heading = "Sunspots")]
    pub sunspot_penumbra: f32,
    /// Smallest spot to keep, in millionths of the visible hemisphere.
    #[arg(long, value_name = "MSH", default_value_t = helia_core::sunspots::Params::default().min_msh, help_heading = "Sunspots")]
    pub sunspot_min_area: f32,
    /// Largest spot, in millionths of the hemisphere; a dark region bigger than
    /// this (a cloud, a vignette) is left out.
    #[arg(long, value_name = "MSH", default_value_t = helia_core::sunspots::Params::default().max_msh, help_heading = "Sunspots")]
    pub sunspot_max_area: f32,
    /// Spots whose edges are closer than this on the Sun, in degrees, form one
    /// group.
    #[arg(long, value_name = "DEG", default_value_t = helia_core::sunspots::Params::default().group_gap_deg, help_heading = "Sunspots")]
    pub sunspot_group_gap: f32,
    /// Observer factor k of the Wolf number R = k (10 g + s).
    #[arg(long, value_name = "K", default_value_t = helia_core::sunspots::Params::default().k, help_heading = "Sunspots")]
    pub wolf_k: f32,
    /// How far solar north is turned counter-clockwise from the top of the
    /// image, in degrees. Default 0: solar north up.
    #[arg(
        long,
        value_name = "DEG",
        default_value_t = 0.0,
        allow_negative_numbers = true,
        help_heading = "Sunspots"
    )]
    pub north_angle: f32,
    /// The image has celestial north up (an equatorial mount): solar north is
    /// then turned by the position angle P of the Sun at the capture time, added
    /// to --north-angle.
    #[arg(long, help_heading = "Sunspots")]
    pub celestial_north_up: bool,
    /// The image is a mirror image (west is on the left).
    #[arg(long, help_heading = "Sunspots")]
    pub mirror: bool,
}

impl SunspotTuning {
    /// The detection parameters these options give.
    #[must_use]
    pub fn params(&self) -> helia_core::sunspots::Params {
        helia_core::sunspots::Params {
            umbra: self.sunspot_umbra,
            penumbra: self.sunspot_penumbra,
            min_msh: self.sunspot_min_area,
            max_msh: self.sunspot_max_area,
            group_gap_deg: self.sunspot_group_gap,
            k: self.wolf_k,
            rotate_deg: self.north_angle,
            mirror: self.mirror,
            celestial_north_up: self.celestial_north_up,
        }
    }
}

/// Options of `helia sunspots`.
#[derive(clap::Args)]
pub struct SunspotsArgs {
    /// Stacked image: .tif, .tiff, .png or .fits (mono or RGB; RGB is
    /// averaged to grey), white light, with the whole disc in view.
    pub input: PathBuf,
    /// When the image was taken, UTC, such as 2026-10-20T11:30:00. Gives P, B0
    /// and L0 for the Carrington longitude. Without it, a FITS file's DATE-AVG
    /// or DATE-OBS is used, else B0 is taken as 0.
    #[arg(long, value_name = "TIME", conflicts_with = "capture")]
    pub date: Option<String>,
    /// The capture (a .ser) the image was stacked from: its frame timestamps
    /// give the time.
    #[arg(long, value_name = "FILE")]
    pub capture: Option<PathBuf>,
    /// Write the spot table as CSV.
    #[arg(long, value_name = "FILE")]
    pub csv: Option<PathBuf>,
    /// Write the whole report as JSON.
    #[arg(long, value_name = "FILE")]
    pub json: Option<PathBuf>,
    /// Write the overlay picture (spots outlined on the flattened disc) as PNG.
    #[arg(long, value_name = "FILE")]
    pub overlay: Option<PathBuf>,
    #[command(flatten)]
    /// Detection tuning.
    pub tuning: SunspotTuning,
}

/// How the brightness of a time-lapse is evened out.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeflickerArg {
    /// Leave the brightness alone.
    Off,
    /// Bring every frame to the median brightness of the series.
    Match,
    /// Bring every frame to the median of the frames around it
    /// (`--deflicker-window`): flicker goes, a slow change stays.
    Smooth,
}

/// Tuning of the solar derotation (`helia solar-derotate`, `helia timelapse
/// --derotate`).
#[derive(clap::Args, Clone, Debug)]
pub struct DerotateTuning {
    /// The rotation law: howard (Howard, Harvey and Forgach 1990, sunspots and
    /// plage), snodgrass (Snodgrass and Ulrich 1990, Doppler features) or your
    /// own A,B,C in degrees a day for A + B sin^2 + C sin^4 of the latitude.
    #[arg(long, value_name = "LAW", default_value = "howard", value_parser = clap::value_parser!(helia_core::derotate::RotationLaw), help_heading = "Derotation")]
    pub rotation_law: helia_core::derotate::RotationLaw,
    /// How far solar north is turned counter-clockwise from the top of the
    /// image, in degrees. Default 0: solar north up.
    #[arg(
        long,
        value_name = "DEG",
        default_value_t = 0.0,
        allow_negative_numbers = true,
        help_heading = "Derotation"
    )]
    pub north_angle: f32,
    /// The images have celestial north up (an equatorial mount): solar north is
    /// then turned by the position angle P of the Sun at each image's time, added
    /// to --north-angle.
    #[arg(long, help_heading = "Derotation")]
    pub celestial_north_up: bool,
    /// The images are mirror images (west is on the left).
    #[arg(long, help_heading = "Derotation")]
    pub mirror: bool,
    /// Move the pixel values as they are. By default the contrast against the
    /// limb darkening goes with each feature, so a feature that moves toward the
    /// limb is not dimmed twice.
    #[arg(long, help_heading = "Derotation")]
    pub raw_brightness: bool,
}

impl DerotateTuning {
    /// The orientation these options give.
    #[must_use]
    pub fn orientation(&self) -> helia_core::derotate::Orientation {
        helia_core::derotate::Orientation {
            rotate_deg: self.north_angle,
            mirror: self.mirror,
            celestial_north_up: self.celestial_north_up,
        }
    }

    /// The derotation of a series these options give, to `target`.
    #[must_use]
    pub fn derotation(
        &self,
        target: helia_core::timelapse::DerotateTarget,
    ) -> helia_core::timelapse::Derotation {
        helia_core::timelapse::Derotation {
            law: self.rotation_law,
            orient: self.orientation(),
            target,
            preserve_contrast: !self.raw_brightness,
        }
    }
}

/// Options of `helia mosaic`.
#[derive(clap::Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent command-line switch"
)]
pub struct MosaicArgs {
    /// The panels: captures (.ser, .avi, a video) to stack, or images already
    /// stacked (.tif, .tiff, .png, .fits); at least two, each sharing a strip of
    /// the Sun with a neighbour. The first is the reference: the mosaic is in
    /// its orientation and brightness.
    #[arg(required = true, num_args = 1.., value_name = "PANELS")]
    pub inputs: Vec<PathBuf>,
    /// The mosaic to write; the extension picks the format (.tif, .png or .fits).
    /// The panel and pair tables are written beside it as NAME-panels.csv and
    /// NAME-pairs.csv, and the stacks made from captures in NAME-stacks/.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// The most the panels may be turned against each other, in degrees; 0
    /// holds them all square to the first.
    #[arg(long, value_name = "DEG", default_value_t = 4.0)]
    pub max_rotation: f64,
    /// The least overlap that counts, as a percentage of the smaller panel.
    #[arg(long, value_name = "PERCENT", default_value_t = 5.0)]
    pub min_overlap: f32,
    /// The least normalised correlation of the detail on an overlap for a pair to
    /// be believed, 0 to 1.
    #[arg(long, value_name = "R", default_value_t = 0.45)]
    pub min_correlation: f32,
    /// Width in pixels of the blend across a seam and at a panel's edge.
    #[arg(long, value_name = "PX", default_value_t = 12.0)]
    pub feather: f32,
    /// Do not match the panels' gain and offset on their overlaps.
    #[arg(long)]
    pub no_gain_match: bool,
    /// Do not search for seams of least difference: blend by the distance from
    /// each panel's edge.
    #[arg(long)]
    pub no_seams: bool,
    /// Bring every panel to one time by the Sun's differential rotation before
    /// joining them (needs the time of every panel and enough of the limb in the
    /// mosaic to fit the disc). By default to the middle time; see --derotate-to.
    #[arg(long, help_heading = "Derotation")]
    pub derotate: bool,
    /// The time to derotate to: first, middle or last (of the panels, by time), or
    /// a UTC time such as 2026-10-20T14:30:00 (implies --derotate).
    #[arg(long, value_name = "TIME", help_heading = "Derotation")]
    pub derotate_to: Option<String>,
    #[command(flatten)]
    /// The rotation law and the orientation of the panels.
    pub derotation: DerotateTuning,
    /// The UTC date of the panels (YYYY-MM-DD) for file names that hold only the
    /// time of day.
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,
    /// Hours east of UTC of the clock that named the files, when their names
    /// give local time.
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    pub utc_offset: f64,
    /// Use a stack already in NAME-stacks instead of stacking its capture again.
    #[arg(long)]
    pub reuse_stacks: bool,
    /// Stack settings, after `--`: everything `helia stack` takes except the
    /// input and the output. Every capture is stacked with the same ones.
    #[arg(last = true, value_name = "STACK OPTIONS")]
    pub stack: Vec<String>,
}

/// Options of `helia solar-derotate`.
#[derive(clap::Args)]
pub struct SolarDerotateArgs {
    /// The images to derotate (.tif, .tiff, .png, .fits): stacks, or panels of
    /// a mosaic. Each needs a time (see --date) and the whole disc in view.
    #[arg(required = true, num_args = 1.., value_name = "IMAGES")]
    pub inputs: Vec<PathBuf>,
    /// Directory for the derotated images, named NAME-derotated.EXT; created
    /// when it does not exist.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
    /// The time to bring every image to: first, middle or last (of the
    /// images, by time), or a UTC time such as 2026-10-20T14:30:00.
    #[arg(long, value_name = "TIME", default_value = "middle")]
    pub to: String,
    /// The UTC date of the images (YYYY-MM-DD) for file names that hold only the
    /// time of day.
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,
    /// Hours east of UTC of the clock that named the files, when their names
    /// give local time.
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    pub utc_offset: f64,
    #[command(flatten)]
    /// The rotation law and the orientation of the images.
    pub tuning: DerotateTuning,
}

/// Options of `helia timelapse`.
#[derive(clap::Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent command-line switch"
)]
pub struct TimelapseArgs {
    /// The captures to stack (.ser, .avi, or a video), or images already stacked
    /// (.tif, .tiff, .png, .fits): one per frame. They are put in time order
    /// when every one has a time (see --date), else kept as given.
    #[arg(required = true, num_args = 1.., value_name = "CAPTURES")]
    pub inputs: Vec<PathBuf>,
    /// Directory for the output: frame-0001.png and so on, timelapse.mp4 (or
    /// timelapse.png when it is an animated PNG), timelapse.csv, and the stacks
    /// in stacks/. Created when it does not exist.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
    /// Write the diagnostics of the series into this directory (created): how
    /// each frame was registered (shift, method, limb fit confidence, phase
    /// correlation), the brightness before and after `--deflicker`, and, with
    /// `--derotate`, how far the Sun's rotation moved the surface of each
    /// frame; each as a PNG with its numbers as CSV and JSON, and
    /// `report.html`.
    #[arg(long, value_name = "DIR")]
    pub report: Option<PathBuf>,
    /// Format of the numbered frames: png (8-bit, for sharing) or tiff (16-bit).
    #[arg(long, value_name = "FORMAT", default_value = "png", value_parser = clap::value_parser!(helia_core::timelapse::FrameFormat))]
    pub format: helia_core::timelapse::FrameFormat,
    /// The movie: auto (MP4 when ffmpeg is installed, else an animated PNG), mp4
    /// (an error without ffmpeg), apng (needs nothing else) or none.
    #[arg(long, value_name = "KIND", default_value = "auto", value_parser = clap::value_parser!(helia_core::animate::MovieChoice))]
    pub movie: helia_core::animate::MovieChoice,
    /// Frames a second of the movie.
    #[arg(long, value_name = "FPS", default_value_t = 10.0)]
    pub fps: f32,
    /// The frame the others are registered to: first, middle or a number from 1.
    #[arg(long, value_name = "FRAME", default_value = "middle")]
    pub reference: String,
    /// Register by the limb fit alone, without the phase correlation of the disc
    /// centre that refines it.
    #[arg(long)]
    pub no_refine: bool,
    /// Largest correction in pixels the phase correlation may make to the limb
    /// shift; a bigger one is taken for a false lock and ignored.
    #[arg(long, value_name = "PX", default_value_t = 3.0)]
    pub max_refine: f32,
    /// Even out the brightness of the frames: off, match or smooth.
    #[arg(long, value_enum, default_value = "off")]
    pub deflicker: DeflickerArg,
    /// Frames in the running median of --deflicker smooth (odd, at least 3).
    #[arg(long, value_name = "N", default_value_t = 5)]
    pub deflicker_window: usize,
    /// Cut every frame to the area all of them cover, so there are no empty
    /// edges where a frame was moved.
    #[arg(long)]
    pub crop: bool,
    /// The UTC date of the captures (YYYY-MM-DD) for file names that hold only
    /// the time of day, such as `SharpCap`'s `15_32_09.avi`.
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,
    /// Hours east of UTC of the clock that named the files (1 for British Summer
    /// Time), when their names give local time.
    #[arg(
        long,
        value_name = "HOURS",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    pub utc_offset: f64,
    /// Bring every frame to one time by the Sun's differential rotation, so the
    /// surface features hold still as the series plays (needs the time of every
    /// capture). By default to the time of the reference frame; see
    /// --derotate-to.
    #[arg(long, help_heading = "Derotation")]
    pub derotate: bool,
    /// The time to derotate to, UTC, such as 2026-10-20T14:30:00 (implies
    /// --derotate).
    #[arg(long, value_name = "TIME", help_heading = "Derotation")]
    pub derotate_to: Option<String>,
    #[command(flatten)]
    /// The rotation law and the orientation of the images.
    pub derotation: DerotateTuning,
    /// Keep the captures in the order given instead of sorting them by time.
    #[arg(long)]
    pub no_sort: bool,
    /// Use a stack already in DIR/stacks instead of stacking its capture again
    /// (to try other registration settings quickly).
    #[arg(long)]
    pub reuse_stacks: bool,
    /// Stack settings, after `--`: everything `helia stack` takes except the
    /// input and the output, such as `-- --preset ha --keep 25%`. Every capture
    /// is stacked with the same ones.
    #[arg(last = true, value_name = "STACK OPTIONS")]
    pub stack: Vec<String>,
}

#[derive(Parser)]
#[command(name = "helia stack", no_binary_name = true)]
struct StackOnly {
    #[command(flatten)]
    args: StackArgs,
}

/// The arguments of `helia stack` for one capture of a series: `extra` are the
/// settings given after `--`.
///
/// # Errors
/// The clap error when the settings do not parse.
pub fn stack_args_for(
    input: &std::path::Path,
    output: &std::path::Path,
    extra: &[String],
) -> Result<StackArgs, clap::Error> {
    let mut v: Vec<std::ffi::OsString> =
        vec![input.into(), "-o".into(), output.into(), "--quiet".into()];
    v.extend(extra.iter().map(Into::into));
    StackOnly::try_parse_from(v).map(|s| s.args)
}

/// Options of `helia lut`.
#[derive(clap::Args)]
pub struct LutArgs {
    /// The filter whose colour look to write (ha, cak, cah, mg, na, he-d3,
    /// hbeta, white); see `helia presets`.
    #[arg(value_parser = clap::value_parser!(Preset))]
    pub preset: Preset,
    /// Output file. The extension picks the format: .cube, .csv, .lut
    /// (`ImageJ`) or .png, unless --format says.
    #[arg(short, long)]
    pub output: PathBuf,
    /// File format: cube, csv, imagej or png.
    #[arg(long, value_parser = clap::value_parser!(helia_core::lut_export::LutFormat))]
    pub format: Option<helia_core::lut_export::LutFormat>,
    /// Entries in the table, 2 to 4096 (an `ImageJ` table always has 256).
    #[arg(long, value_name = "N", default_value_t = 256)]
    pub size: usize,
    /// Invert the picture before colouring (the table runs from white to black).
    #[arg(long)]
    pub invert: bool,
    /// Mix of colour and grey from 0 to 1.
    #[arg(long, value_name = "S", default_value_t = 1.0)]
    pub strength: f32,
}

/// What kind of feedback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum FeedbackKind {
    /// Something is broken or wrong.
    #[default]
    Bug,
    /// A request for something new.
    Feature,
    /// A question about using Helia.
    Question,
    /// Anything else.
    Other,
}

/// Options of `helia feedback`.
///
/// Nothing is sent until you run the command; `--dry-run` shows exactly what
/// would be sent and sends nothing. Helia never sends anything else, and has
/// no telemetry.
#[derive(clap::Args)]
pub struct FeedbackArgs {
    /// What kind of feedback this is.
    #[arg(long, value_enum, default_value_t = FeedbackKind::Bug)]
    pub kind: FeedbackKind,
    /// One line that says what it is (at most 120 characters).
    #[arg(long)]
    pub title: String,
    /// The details (at most 8000 characters). `-` reads standard input.
    #[arg(long, default_value = "")]
    pub description: String,
    /// A PNG screenshot to attach (at most 5 MiB; larger ones are shrunk).
    /// Check it shows nothing private: it is filed in the issue tracker.
    #[arg(long, value_name = "FILE.png")]
    pub screenshot: Option<PathBuf>,
    /// A rectangle to highlight on the screenshot, as `x,y,w,h` in its pixels
    /// (repeatable).
    #[arg(long, value_name = "X,Y,W,H", requires = "screenshot")]
    pub highlight: Vec<String>,
    /// Include system details: Helia version, commit and target, and your
    /// operating system. Never a host name, user name or path.
    #[arg(long)]
    pub system_info: bool,
    /// Feedback service address (default: `HELIA_FEEDBACK_URL`, else the
    /// public service at feedback.discworld.casa).
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,
    /// Show the report that would be sent, and send nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// If the service cannot be reached, also save the report to this `.zip`.
    #[arg(long, value_name = "FILE.zip")]
    pub save: Option<PathBuf>,
}

/// Options of `helia serve`.
#[derive(clap::Args)]
pub struct ServeArgs {
    /// Address to listen on; a non-loopback address needs a token.
    #[arg(long, default_value = "127.0.0.1:7870")]
    pub bind: SocketAddr,
    /// Directory all request paths must stay inside (default: the current directory).
    #[arg(long, value_name = "DIR")]
    pub data_root: Option<PathBuf>,
    /// Bearer token. Visible in the process list; prefer --token-file or the
    /// `HELIA_TOKEN` environment variable.
    #[arg(long, conflicts_with = "token_file")]
    pub token: Option<String>,
    /// File holding the bearer token.
    #[arg(long, value_name = "FILE")]
    pub token_file: Option<PathBuf>,
    /// Jobs that run at once.
    #[arg(long, default_value_t = 1)]
    pub workers: usize,
    /// Queued plus running jobs allowed before answering 429.
    #[arg(long, default_value_t = 16)]
    pub max_queue: usize,
    /// Allow browser access from this origin (repeatable). Off by default.
    #[arg(long, value_name = "ORIGIN")]
    pub cors_origin: Vec<String>,
    /// Directory for compressed video decoded by ffmpeg (default: the operating
    /// system's cache directory, or `HELIA_CACHE_DIR`).
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// Accept this Host header on a loopback bind (repeatable), e.g. behind a proxy.
    #[arg(long, value_name = "HOST")]
    pub allow_host: Vec<String>,
}

/// Options of `helia calibrate`.
///
/// Dust on the sensor window, an etalon's fringes and a camera's fixed pattern
/// stay put while the Sun moves, and sharpening makes them worse. Capture some
/// darks (lens capped, same exposure and gain as the Sun), some flats (an even
/// field, same focus and set-up as the Sun) and, for the flats, darks at the
/// flats' exposure; this builds the masters from them. Each input is a SER,
/// AVI or video file, a FITS still, or a folder of them.
#[derive(clap::Args)]
pub struct CalibrateArgs {
    /// Dark captures (lens capped, same exposure and gain as the Sun).
    #[arg(long, value_name = "PATH", num_args = 1..)]
    pub darks: Vec<PathBuf>,
    /// Flat captures (an even field, at the focus and set-up of the Sun).
    #[arg(long, value_name = "PATH", num_args = 1..)]
    pub flats: Vec<PathBuf>,
    /// Captures of the Sun at many places on the sensor (the mount off tracking,
    /// or nudged between frames), to solve the flat from instead of from
    /// `--flats`: it then holds the dust and fringes of the observing moment,
    /// without a diffuser. The darks (same exposure as the Sun) are taken off
    /// first. Needs at least six frames with the limb clear.
    #[arg(long, value_name = "PATH", num_args = 1.., conflicts_with = "flats")]
    pub displaced: Vec<PathBuf>,
    /// With `--displaced`, keep every scale of the solution, not only the
    /// structure finer than a quarter of the displacements (the smooth scales
    /// cannot be told from the Sun's own limb darkening unless it moved far).
    #[arg(long, requires = "displaced")]
    pub displaced_keep_smooth: bool,
    /// Dark captures at the exposure of the flats. Without them the flats keep
    /// their offset (or lose the bias, if `--bias` is given).
    #[arg(long, value_name = "PATH", num_args = 1.., alias = "dark-flats")]
    pub flat_darks: Vec<PathBuf>,
    /// Bias captures (the shortest exposure). Only needed to scale a dark to
    /// another exposure.
    #[arg(long, value_name = "PATH", num_args = 1..)]
    pub bias: Vec<PathBuf>,
    /// A light capture: pixels that read the same wrong value in every frame
    /// while the Sun moves (stuck pixels) are added to the bad-pixel map.
    /// Useful when there are no darks.
    #[arg(long, value_name = "CAPTURE")]
    pub lights: Option<PathBuf>,
    /// Folder for `master-dark.fits`, `master-flat.fits`, `master-bias.fits`
    /// and `bad-pixels.fits` (created if needed).
    #[arg(short, long, value_name = "DIR", default_value = "calibration")]
    pub output_dir: PathBuf,
    /// How frames are combined: median, sigma (clip at 3 standard
    /// deviations) or sigma:K.
    #[arg(long, default_value = "sigma", value_parser = clap::value_parser!(helia_core::calibrate::Combine))]
    pub combine: helia_core::calibrate::Combine,
    /// Use at most this many frames per master, spread over the input (default:
    /// as many as fit in 3 GiB).
    #[arg(long, value_name = "N")]
    pub max_frames: Option<usize>,
    /// Do not look for hot, cold and stuck pixels.
    #[arg(long)]
    pub no_bad_pixels: bool,
    /// A pixel this many robust standard deviations off its neighbours in the
    /// dark is hot.
    #[arg(long, value_name = "K", default_value_t = 6.0)]
    pub hot_sigma: f32,
    /// The filter the flats were taken through (ha, cak, ...): adds the advice
    /// that goes with it, such as the etalon's sweet spot and Newton rings for
    /// H-alpha.
    #[arg(long, value_name = "FILTER", value_parser = clap::value_parser!(Preset))]
    pub preset: Option<Preset>,
    #[command(flatten)]
    /// How to read the captures, instead of what their headers say.
    pub open: OpenArgs,
    /// Hide the progress messages.
    #[arg(short, long)]
    pub quiet: bool,
}

/// Calibration options of `helia stack`.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct CalibrationArgs {
    /// Master dark (FITS from `helia calibrate`, or any FITS or TIFF), or dark
    /// captures to build one from (a SER, AVI or FITS file, or a folder).
    /// Subtracted from every frame before it is demosaiced or aligned.
    #[arg(long, value_name = "PATH")]
    pub dark: Option<PathBuf>,
    /// Master flat, or flat captures to build one from. Divided out of every
    /// frame after the dark.
    #[arg(long, value_name = "PATH")]
    pub flat: Option<PathBuf>,
    /// Captures of the Sun at many places on the sensor, to solve a flat from
    /// (see `helia calibrate --displaced`); used when no `--flat` is given. The
    /// dark is taken off first.
    #[arg(long, value_name = "PATH", conflicts_with = "flat")]
    pub displaced: Option<PathBuf>,
    /// With `--displaced`, keep every scale of the solution (see
    /// `helia calibrate --displaced-keep-smooth`).
    #[arg(long, requires = "displaced")]
    pub displaced_keep_smooth: bool,
    /// Dark captures at the exposure of the flats, when `--flat` is captures.
    #[arg(long, value_name = "PATH", alias = "dark-flat")]
    pub flat_dark: Option<PathBuf>,
    /// Master bias (or captures). Only used with `--scale-dark`.
    #[arg(long, value_name = "PATH")]
    pub bias: Option<PathBuf>,
    /// Bad-pixel map: `bad-pixels.fits` from `helia calibrate`, or a text file
    /// with an `x y` pair (from 0, top-left) per line. Without it, hot pixels
    /// of the dark and dead pixels of the flat are found and corrected.
    #[arg(long, value_name = "FILE")]
    pub bad_pixels: Option<PathBuf>,
    /// Do not look for hot and cold pixels in the dark and the flat.
    #[arg(long)]
    pub no_cosmetic: bool,
    /// Scale the dark by the ratio of the exposures (the Sun's exposure comes
    /// from the `SharpCap` sidecar). Best with `--bias`.
    #[arg(long)]
    pub scale_dark: bool,
    /// How frames are combined into a master: median, sigma or sigma:K.
    #[arg(long, default_value = "sigma", value_parser = clap::value_parser!(helia_core::calibrate::Combine))]
    pub calibration_combine: helia_core::calibrate::Combine,
}

impl CalibrationArgs {
    /// True when any calibration is asked for.
    #[must_use]
    pub fn any(&self) -> bool {
        self.dark.is_some()
            || self.flat.is_some()
            || self.displaced.is_some()
            || self.bad_pixels.is_some()
    }

    /// The masters to resolve.
    #[must_use]
    pub fn spec(&self, open: OpenOptions) -> helia_core::calibrate::CalibrationSpec {
        helia_core::calibrate::CalibrationSpec {
            dark: self.dark.clone(),
            bias: self.bias.clone(),
            flat: self.flat.clone(),
            dark_flat: self.flat_dark.clone(),
            displaced: self.displaced.clone(),
            displaced_keep_smooth: self.displaced_keep_smooth,
            bad_pixels: self.bad_pixels.clone(),
            cosmetic: !self.no_cosmetic,
            scale_dark: self.scale_dark,
            combine: self.calibration_combine,
            open,
            ..helia_core::calibrate::CalibrationSpec::default()
        }
    }
}

/// Options that say how to read a capture, instead of what its header says.
/// Shared by `helia stack` and `helia info`.
#[derive(clap::Args, Clone, Copy, Debug)]
pub struct OpenArgs {
    /// Bayer handling: auto, none, rggb, bggr, grbg or gbrg. auto trusts the
    /// header, but for a SER file labelled mono it also checks the pixels for a
    /// hidden Bayer mosaic (some recorders label raw colour data as mono) and
    /// debayers when it finds one. none treats the frames as mono whatever the
    /// header or sidecar say; a pattern forces it. For AVI the pattern otherwise
    /// comes from the `SharpCap` sidecar. For FITS frames auto takes BAYERPAT
    /// (as stored) and ROWORDER from the header.
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(BayerSetting))]
    pub bayer: BayerSetting,
    /// Bits per sample, instead of the SER header's: 1 to 8 read one byte per
    /// pixel, 9 to 16 two bytes. For a header that gets the depth wrong. SER only.
    #[arg(long, value_name = "BITS", value_parser = clap::value_parser!(u32).range(1..=16))]
    pub bit_depth: Option<u32>,
    /// Byte order of 16-bit samples, instead of the SER header flag (little or
    /// big). SER only.
    #[arg(long, value_name = "ORDER", value_parser = clap::value_parser!(Endian))]
    pub endian: Option<Endian>,
    /// Read the rows bottom to top, for a file stored upside down. Patterns
    /// given to --bayer describe the picture after the flip. SER only.
    #[arg(long)]
    pub flip_rows: bool,
}

impl OpenArgs {
    /// The options for [`helia_core::io::source::open_capture`].
    #[must_use]
    pub fn options(&self) -> OpenOptions {
        OpenOptions {
            bayer: self.bayer,
            bit_depth: self.bit_depth,
            endian: self.endian,
            flip_rows: self.flip_rows,
            ..OpenOptions::default()
        }
    }
}

/// What `--colour` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourChoice {
    /// The preset that was chosen or detected.
    Auto,
    /// No colour: greyscale.
    Mono,
    /// True colour from a raw colour camera: red, green and blue stacked and
    /// white balanced.
    True,
    /// This preset's colours.
    Preset(Preset),
}

impl std::str::FromStr for ColourChoice {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "mono" | "none" | "grey" | "gray" => Ok(Self::Mono),
            "true" | "truecolour" | "truecolor" | "rgb" => Ok(Self::True),
            _ => s.parse::<Preset>().map(Self::Preset).map_err(|_| {
                let ids: Vec<&str> = Preset::ALL.iter().map(|p| p.id()).collect();
                Error::Invalid(format!(
                    "unknown colour {s:?}; use auto, mono, true or one of {}",
                    ids.join(", ")
                ))
            }),
        }
    }
}

/// How samples at one output pixel are combined beyond the weighted mean.
#[derive(Clone, Copy, ValueEnum)]
pub enum RobustArg {
    /// Weighted mean of every sample (the default).
    None,
    /// Drop samples further than K sigma from the mean (default K 2.5).
    Sigma,
    /// Huber M-estimator: down-weight samples beyond K sigma in proportion to
    /// their distance (default K 1.345).
    Huber,
    /// Winsorised mean: clamp samples to K sigma from the mean (default K 2).
    Winsor,
}

/// A switch spelled out.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    /// Enabled.
    On,
    /// Disabled.
    Off,
}

/// Colour plane to analyse and stack from a Bayer capture.
#[derive(Clone, Copy, ValueEnum)]
pub enum ChannelArg {
    /// Red plane only (usual for H-alpha).
    R,
    /// Green plane only.
    G,
    /// Blue plane only.
    B,
    /// Mean of the three planes.
    Lum,
}

/// Demosaicing method.
#[derive(Clone, Copy, ValueEnum)]
pub enum DebayerArg {
    /// Plain bilinear interpolation.
    Bilinear,
    /// Malvar-He-Cutler, edge aware.
    Mhc,
}

/// Frame sharpness metric.
#[derive(Clone, Copy, ValueEnum)]
pub enum MetricArg {
    /// Gradient energy.
    Gradient,
    /// Laplacian variance.
    Laplacian,
    /// Helmli and Scherer's mean (CPU only).
    Helmli,
    /// Median-filter gradient similarity (CPU only).
    Mfgs,
    /// DCT energy ratio (CPU only).
    Dct,
    /// Band-limited Fourier power less a noise floor (CPU only).
    Fourier,
    /// Mean local RMS contrast (CPU only).
    Contrast,
}

/// Options of `helia plot`.
#[derive(clap::Args)]
pub struct PlotArgs {
    /// Which figure: quality-sorted, quality-order, quality-histogram,
    /// shift-drift, seeing-time, ap-grid, ap-quality, ap-shift, ap-field,
    /// ap-residual, power-spectrum, mtf, limb-profile, limb-psf, histogram,
    /// noise-snr, grid-stonyhurst, grid-carrington, timelapse-registration,
    /// timelapse-brightness or timelapse-derotation; or `all`.
    #[arg(value_name = "KIND", value_parser = parse_plot_kind)]
    pub kind: PlotChoice,
    /// A report directory written by `helia stack --report`, or a capture
    /// (.ser, .avi, a video, FITS frames), which is stacked first with the options
    /// after `--`.
    pub input: PathBuf,
    /// Where to write the PNG (default: the figure's name in the current
    /// directory). With `all`, a directory (default: the current one).
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Size in pixels, `WIDTHxHEIGHT`.
    #[arg(long, value_name = "WxH", default_value = "960x540", value_parser = parse_size)]
    pub size: (u32, u32),
    /// Options for `helia stack` when INPUT is a capture, after `--`, for
    /// example `-- --keep 10% --preset ha`.
    #[arg(last = true, value_name = "STACK OPTIONS")]
    pub stack_options: Vec<String>,
}

/// One figure, or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlotChoice {
    /// Every figure with data.
    All,
    /// One figure.
    One(helia_report::PlotKind),
}

fn parse_plot_kind(s: &str) -> Result<PlotChoice, String> {
    if s == "all" {
        return Ok(PlotChoice::All);
    }
    s.parse::<helia_report::PlotKind>().map(PlotChoice::One)
}

fn parse_utc(s: &str) -> Result<f64, String> {
    helia_core::io::solarnet::unix_from_iso(s)
        .ok_or_else(|| format!("cannot read the time {s:?}; use YYYY-MM-DDThh:mm:ss in UTC"))
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
    let p = |v: &str| v.trim().parse::<u32>().map_err(|e| format!("{v:?}: {e}"));
    let (w, h) = (p(w)?, p(h)?);
    if !(200..=8000).contains(&w) || !(150..=8000).contains(&h) {
        return Err("the size must be 200 to 8000 wide and 150 to 8000 high".into());
    }
    Ok((w, h))
}

/// Options of `helia stack`.
#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("seeing_probe").multiple(true).args(["seeing", "report"])))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent command-line switch"
)]
pub struct StackArgs {
    /// Capture file: .ser or .avi, or a compressed video (.mp4, .mov, .mkv, .webm,
    /// .m4v) read through the optional ffmpeg program, or FITS frames: a .fit,
    /// .fits or .fts file (a cube, or several images in one file), a folder of
    /// them, or a pattern such as 'Light_*.fit' (in quotes). The frames of a
    /// folder or pattern are stacked in DATE-OBS order, else file name order, and
    /// must all be the same size.
    pub input: PathBuf,
    /// Output image; the extension picks the format (.tif/.tiff, .png or .fits/.fit/.fts).
    #[arg(short, long)]
    pub output: PathBuf,
    /// Share of frames to keep, e.g. 25%.
    #[arg(long, value_parser = clap::value_parser!(Keep), conflicts_with = "keep_frames")]
    pub keep: Option<Keep>,
    /// Number of frames to keep.
    #[arg(long, value_name = "N")]
    pub keep_frames: Option<usize>,
    /// Alignment point size in pixels. Default: chosen from the image size.
    #[arg(long, conflicts_with = "ap_auto")]
    pub ap_size: Option<usize>,
    /// Choose the alignment point size, spacing and search range from the image
    /// scale (the fitted limb) and the seeing (a probe of a dozen frames),
    /// instead of one size for every capture. What was chosen is printed.
    #[arg(long, conflicts_with = "no_multipoint")]
    pub ap_auto: bool,
    /// Alignment point passes: 1 measures the points against the reference
    /// built from the best few frames, 2 stacks once and measures them again
    /// against that stack (less noise, more detail to lock on). Needs
    /// multipoint. Default: 1.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..=2), conflicts_with = "no_multipoint")]
    pub ap_passes: Option<u8>,
    /// Replace alignment point shifts that disagree with their neighbours (a
    /// mismatch on a neighbouring granule) by the neighbours' median. Needs
    /// multipoint. Default: off.
    #[arg(long, value_name = "on|off", conflicts_with = "no_multipoint")]
    pub ap_smooth: Option<OnOff>,
    /// Measure the alignment points once more against the finished stack and
    /// print how far the shifts that were used are from that (px RMS): a check
    /// of the registration, at the cost of one more measuring pass.
    #[arg(long, conflicts_with = "no_multipoint")]
    pub ap_residual: bool,
    /// Crop the result to the solar disc plus a margin (FITS too). A capture
    /// without a clear disc is left as it is, with a note.
    #[arg(long)]
    pub autocrop: bool,
    /// Estimate the seeing from the capture: the Fried parameter r0 from the
    /// spectral ratio of tiles of the disc over a sample of all the frames
    /// (needs --aperture-mm), and a relative index from how much neighbouring
    /// alignment points move against each other. Printed after the stack and
    /// written into the FITS header. Reads about 120 more frames.
    #[arg(long)]
    pub seeing: bool,
    /// Write the science outputs of the stack into this directory (created):
    /// the frame quality graphs sorted and in capture order with the keep
    /// threshold, the quality histogram, the global shift and drift, the
    /// seeing over the capture, the alignment point plots, the power
    /// spectrum, transfer curves, limb profile and blur, histograms and noise,
    /// and the Stonyhurst and Carrington grids (with --sunspots, the sunspot
    /// table too), each as a PNG with its numbers as CSV and JSON, and
    /// `report.html`, one self-contained page with the figures, tables and the
    /// settings used. Turns the seeing probe on (about 120 more frames are
    /// read). Redraw one figure with `helia plot`.
    #[arg(long, value_name = "DIR")]
    pub report: Option<PathBuf>,
    /// Undo the field rotation of an altazimuth mount: every frame is turned to
    /// the orientation of the middle of the capture (or --field-rotation-reference)
    /// from the parallactic angle of the Sun at the frame's SER timestamp, for
    /// the site given by --site or `HELIA_SITE`, before it is scored, aligned or
    /// stacked. Needs a capture with timestamps (SER); not for an equatorial
    /// mount, and not with `--colour true`.
    #[arg(long)]
    pub field_rotation: bool,
    /// The orientation --field-rotation turns every frame to: middle (the
    /// default), first or last.
    #[arg(
        long,
        value_name = "WHICH",
        default_value = "middle",
        requires = "field_rotation",
        value_parser = clap::value_parser!(helia_core::field_rotation::Reference)
    )]
    pub field_rotation_reference: helia_core::field_rotation::Reference,
    /// The picture is a mirror image (an odd number of reflections, a star
    /// diagonal for one): --field-rotation then turns the other way.
    #[arg(long, requires = "field_rotation")]
    pub field_rotation_mirror: bool,
    /// Find the sunspots on the stack: groups, the relative Wolf number, areas
    /// corrected for foreshortening. Printed after the stack, written into the
    /// FITS header, and saved beside the output as NAME-sunspots.csv, .json and
    /// .png. For a white-light capture with the whole disc in view; the time is
    /// the capture's.
    #[arg(long)]
    pub sunspots: bool,
    /// When the capture was taken, UTC (`YYYY-MM-DDThh:mm:ss`), for a capture
    /// that does not hold its own time (an AVI or a video; a SER's time stamps
    /// are used otherwise). Gives P, B0 and L0 to `--sunspots` and to the
    /// heliographic grids of `--report`.
    #[arg(long, value_name = "TIME", value_parser = parse_utc)]
    pub date: Option<f64>,
    #[command(flatten)]
    /// Detection tuning.
    pub sunspot: SunspotTuning,
    /// Wavelength in nanometres for --seeing and --report when the filter is not known (the
    /// filter's own wavelength is used otherwise).
    #[arg(long, value_name = "NM", requires = "seeing_probe")]
    pub wavelength_nm: Option<f32>,
    /// Telescope aperture in millimetres. With it, the granulation report of a
    /// white-light or sodium stack also gives the diffraction cut-off and how
    /// much of it the data resolves, and --seeing gives r0 and a grade.
    #[arg(long, value_name = "MM")]
    pub aperture_mm: Option<f32>,
    /// Margin of `--autocrop` as a percentage of the disc radius.
    #[arg(
        long,
        value_name = "PERCENT",
        default_value_t = 10.0,
        requires = "autocrop"
    )]
    pub autocrop_margin: f32,
    /// The filter the capture was taken with (ha, cak, cah, mg, na, he-d3,
    /// hbeta, white): applies its recommended channel and frame share and
    /// colours a .tif or .png output. Without it the filter is read from the
    /// file name, folder or sidecar when possible; see `helia presets`.
    #[arg(long, value_name = "FILTER", value_parser = clap::value_parser!(Preset))]
    pub preset: Option<Preset>,
    /// Colour of the output: auto (the preset), mono, a preset id, or true (true
    /// colour from a raw colour camera: red, green and blue stacked on one
    /// alignment, channels aligned and white balanced, written as an RGB TIFF or
    /// PNG or a three-plane FITS). The other looks colour a .tif or .png from the
    /// mono stack; FITS output is mono for them.
    #[arg(long, value_name = "COLOUR", value_parser = clap::value_parser!(ColourChoice))]
    pub colour: Option<ColourChoice>,
    /// Invert the picture before colouring (shows filaments and prominences).
    #[arg(long)]
    pub invert: bool,
    /// White balance of `--colour true`: auto (make the disc centre neutral),
    /// off (the sensor's own balance) or three gains R,G,B such as 1.9,1,1.5.
    #[arg(long, value_name = "BALANCE", default_value = "auto", value_parser = clap::value_parser!(WhiteBalance), help_heading = "True colour")]
    pub white_balance: WhiteBalance,
    /// With `--colour true`, do not register red and blue to green (atmospheric
    /// dispersion). On by default.
    #[arg(long, help_heading = "True colour")]
    pub no_channel_align: bool,
    /// Mix of colour and grey from 0 to 1.
    #[arg(long, value_name = "S", default_value_t = 1.0)]
    pub colour_strength: f32,
    /// Grade the colour: `hue=DEG,sat=S,gamma=G,black=B,white=W,r=R,g=G,b=B`
    /// (any of them; hue -180 to 180, sat 0 to 2, gamma 0.3 to 3, black 0 to 0.9,
    /// white 0.1 to 1, r/g/b channel curves 0.3 to 3; the default changes
    /// nothing), or the name of a saved colour preset (a file in the colour
    /// presets folder, or a .json file).
    #[arg(long, value_name = "GRADE|PRESET")]
    pub grade: Option<String>,
    /// Observing site as LAT,LON,ALT (degrees north, degrees east, metres), for
    /// a FITS output's OBSGEO-X/Y/Z. Without it the `HELIA_SITE` environment
    /// variable is used, else the keywords are left out.
    #[arg(long, value_name = "LAT,LON,ALT", value_parser = clap::value_parser!(Site))]
    pub site: Option<Site>,
    /// How whole frames are aligned: auto (fit the solar limb when the full disc
    /// is in view and the fit is confident, else phase correlation), phase
    /// (phase correlation, best for a cropped region) or limb (fit the limb;
    /// frames without a usable limb still use phase correlation).
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(AlignMethod))]
    pub align: AlignMethod,
    /// Apply the sharpening and other edits to FITS output too. By default a
    /// FITS file is the unsharpened stack, for further processing elsewhere.
    #[arg(long)]
    pub sharpen_fits: bool,
    /// Sharpening and adjustments, applied before any colour.
    #[command(flatten)]
    pub edits: EditArgs,
    /// Skip multi-point alignment (global alignment only).
    #[arg(long)]
    pub no_multipoint: bool,
    /// Keep alignment and quality scoring off this rectangle, given as x,y,w,h
    /// in pixels (top-left origin); repeat for several. The area is flattened in
    /// every frame, so it is flat in the result too. Burned-in timestamps are
    /// found automatically; use this for anything else (a logo, a hot pixel cluster).
    #[arg(long, value_name = "X,Y,W,H", value_parser = helia_core::mask::parse_region)]
    pub exclude: Vec<helia_core::Rect>,
    /// Place alignment points up to this many pixels outside the fitted limb,
    /// so they can follow prominences (0 keeps them on the disc).
    #[arg(long, value_name = "PX", default_value_t = 0)]
    pub prominence_band: usize,
    /// Do not fit the limb; alignment points may sit anywhere there is contrast.
    #[arg(long)]
    pub no_disc_mask: bool,
    /// Do not look for burned-in text (a capture timestamp) to mask.
    #[arg(long)]
    pub no_overlay_detect: bool,
    /// Compute backend.
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(BackendChoice))]
    pub backend: BackendChoice,
    /// Dark, flat and bad-pixel calibration, applied before demosaicing and
    /// alignment.
    #[command(flatten)]
    pub calibration: CalibrationArgs,
    /// Enable sigma clipping at this many standard deviations (off by default).
    /// Same as `--robust sigma --robust-k K`.
    #[arg(long, value_name = "K", conflicts_with_all = ["robust", "robust_k"])]
    pub sigma_clip: Option<f32>,
    /// Outlier-robust combination of the frames at each pixel: `sigma` drops
    /// outliers, `huber` down-weights them, `winsor` clamps them. Two passes
    /// over the frames. Default `none`, the weighted mean. Removes what
    /// differs between frames (a satellite, a cosmic ray, a hot pixel that
    /// the alignment moves); a defect fixed on the sensor is in the same
    /// place in every frame and needs `--dark` or `--bad-pixels` instead.
    #[arg(long, value_enum)]
    pub robust: Option<RobustArg>,
    /// Threshold of `--robust` in standard deviations. Default: 2.5 for
    /// `sigma`, 1.345 for `huber`, 2 for `winsor`.
    #[arg(long, value_name = "K", requires = "robust")]
    pub robust_k: Option<f32>,
    #[command(flatten)]
    /// How to read the capture, instead of what its header says.
    pub open: OpenArgs,
    /// Colour channel to stack from a Bayer capture. Default: the preset's
    /// channel, else lum.
    #[arg(long, value_enum)]
    pub channel: Option<ChannelArg>,
    /// Demosaicing method.
    #[arg(long, value_enum, default_value = "mhc")]
    pub debayer: DebayerArg,
    /// Sharpness metric used to rank frames.
    #[arg(long, value_enum, default_value = "gradient")]
    pub metric: MetricArg,
    /// Directory for compressed video (.mp4, .mov, .mkv, .webm, compressed .avi) decoded by
    /// ffmpeg. The decoded frames are uncompressed, so this needs room for the whole
    /// video (checked first) and is cleaned up afterwards. Default: the operating
    /// system's cache directory, or the `HELIA_CACHE_DIR` environment variable.
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// Plugin steps and stack backend.
    #[command(flatten)]
    pub plugins: PluginRunArgs,
    /// Hide the progress bar and the summary.
    #[arg(short, long)]
    pub quiet: bool,
    /// Print where the time went: wall time per stage and busy time per kind of work.
    #[arg(long)]
    pub timings: bool,
}

/// `X,Y` as two non-negative pixel coordinates.
fn parse_xy(s: &str) -> Result<(usize, usize), String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    let p = |v: &str| v.trim().parse::<usize>().map_err(|e| format!("{v:?}: {e}"));
    Ok((p(x)?, p(y)?))
}
