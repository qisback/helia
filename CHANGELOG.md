# Changelog

All notable changes to Helia are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

## [0.7.1]

The source link in the About page and the packaging metadata point at GitHub. Adds science outputs groups 3 and 4, a performance and benchmarks page and a Ko-fi button, and makes the release notes terse.

### Fixed

- The source link in the About page (the GPL source offer) and the additional terms point at https://github.com/qisback/helia, and the manual at https://qisback.github.io/helia/. Packaging metadata and crate keywords carry the same links, and the book has search and sharing metadata. 0.7.0 binaries showed a private forge address.

- GUI snapshots no longer change when the version is bumped. How: the GUI reads the version it displays (the step list footer, the About heading, the report figure footer and the feedback details) through one function, `ui_version()`, which returns the fixed string 0.7.0 under `cargo test` and the crate version otherwise. The snapshot pictures are unchanged and the manual screenshots show 0.7.0; the Windows lane no longer fails on a release bump.
- The Stack step Science snapshot in the manual is regenerated: its report footer still read Helia 0.6.0. How: the figure footer carries the crate version, so a version bump changes about 40 pixels of the picture, which sits exactly at the snapshot tolerance on Linux and went over it on the Windows lane. The snapshot is rendered again at 0.7.0.

### Added

- **Performance and benchmarks.** A reproducible benchmark harness and a Performance and benchmarks page with graphs (#215). `tools/bench/bench.py` times `helia stack` end to end on a set of captures and writes one JSON file per machine: wall time per stage from `--timings`, peak resident memory (`ru_maxrss`), peak video memory (`nvidia-smi` at 20 Hz), frames and megabytes a second, CPU, GPU and software-adapter modes, scaling over 1 to 8 cores (`taskset` plus `RAYON_NUM_THREADS`) and over a quarter, a half and all of the frames of a SER capture. It also measures how much CPU the rest of the machine used during each run and marks a run that shared the machine with more than half a core as loaded; `tools/bench/plot.py` leaves those out, takes the best of the rest and draws the SVG graphs and the Markdown tables of the new page. The committed results are in `tools/bench/results`. Measured (best of N, quiet machines, commit f318da8): the 631-frame 1080p SER takes 12.6 s on an 8-vCPU x86 server, 14.7 s on an 8-core Arm server and 2.9 s on an A100; the 5.6 GB, 2713-frame capture 62.2 s on x86 and 8.5 s on the A100 (7.3 times). Apple M1 figures from a separate run are on the page.
- **Charts beside the measured results in the book** (#240). Every table of measured numbers gets a chart. How: the numbers of each chart are copied into a small JSON file in `docs/book/src/data/charts/`, and `tools/docs-graphs/make_graphs.py` (Python 3 standard library only, so the output is reproducible byte for byte) draws them to `docs/book/src/images/charts/*.svg`: Okabe-Ito colours, a card that carries its own background so it reads in any theme, light or dark by `prefers-color-scheme`, text sized for a phone. `cargo xtask docs --check` runs it in check mode and fails when a chart is stale, when a number is not on the page the chart names, or when the page lacks the picture or its caption, so a chart cannot quote a figure the page does not. This first step adds the generator, the check and one chart (the deconvolution recipe); the chapters follow in small pull requests. Nothing in the program changed.
- Science outputs, group 4: solar specifics
  (#199). `helia stack --report DIR` now
  also lays the Stonyhurst and the Carrington heliographic grids over the stack, with P, B0 and L0
  from the ephemeris and solar north and east marked, and, with `--sunspots`, carries the sunspot
  overlay, table and CSV in the same report; `helia timelapse --report DIR` writes the
  time-lapse and derotation diagnostics (registration shift, limb fit confidence and phase
  correlation, brightness before and after `--deflicker`, derotation displacement). New:
  `helia stack --date TIME` (and `date` in an API job) gives the UTC time of a capture that has
  none of its own, such as an AVI, to the grids and to `--sunspots`; `report` in `POST /timelapse`
  and **Save diagnostics** in the Time-lapse step of the app. How: the orientation is the one
  `--sunspots` uses (Meeus, Astronomical Algorithms, chapter 29, about 0.01 degree; the picture
  turned by P for celestial north up, plus `--north-angle`, mirrored for `--mirror`). A point of
  latitude `phi` and longitude `lambda` from the central meridian is visible when
  `sin phi sin B0 + cos phi cos lambda cos B0` is positive and falls at
  `(cos phi sin lambda, sin phi cos B0 - cos phi cos lambda sin B0)` in solar radii, which is
  the inverse of the pixel to latitude conversion the spot table uses (tested: the disc centre
  projects to the centre, the pole to the angle of north, the far side is hidden, east is left
  of north). Carrington meridians are the multiples of 15 degrees of `L0 + lambda`. The
  time-lapse figures are drawn from the per-frame report the registration already keeps
  (`Sequence::report`), so nothing is measured again. The stacked image is unchanged. Measured on
  the owner's SER `15_20_57.ser` (H-alpha, 1920 by 1080): P +4.73, B0 +4.55, L0 54.19 degrees at
  its time stamps' 2022-07-17 14:21:03 UTC, and the grid puts the dark feature near pixel
  (468, 252) at 37 degrees north, 4 east; on the owner's pair `15_32_09.avi` and `15_37_19.avi`
  the diagnostics show a drift of (17.3, -5.9) px in five minutes with a phase lock of 183, a
  brightness spread of 4.2% evened out to 0%, and a derotation of 0.18 px at the largest, so
  derotating a pair this close buys nothing. Tutorial: [reading the solar
  plots](docs/book/src/tutorials/reading-the-solar-plots.md); reference:
  [Science outputs](docs/book/src/user-guide/science-outputs.md); the coverage page now lists
  every output.

- Science outputs, group 3: image science plots
  (#198). `helia stack --report DIR` now
  also draws the radially averaged power spectrum of the stack against the best frame and the
  sharpened image (noise floors, granulation peak and roll-off, sampling limit and the
  diffraction cut-off marked), MTF-like transfer curves between them, the limb-darkening
  profile with the fitted law and its residuals, the limb's measured blur (edge, fitted
  Gaussian step and the PSF it implies), histograms per channel of the written image and of the
  raw capture with the clipping marked, and a noise and signal-to-noise comparison; each with
  its table as CSV and `image-science.json`. The API serves the same files; the app shows the
  stack and best frame curves. How: the spectra are those of the granulation report
  (`granulation::granulation`, the disc-centre square divided by its blur, Hann window, ring
  average) taken on the stack, on the best single frame read from the capture (the plane that
  was stacked, with burned-in text flattened as the pipeline does: the text pulled the disc
  centroid and gave a spectrum four times too contrasty) and on the image after the edits; the
  transfer curve is the square root of the ratio of two powers, drawn against the diffraction
  MTF of a circular aperture, `2/pi (acos x - x sqrt(1 - x^2))` for `x = f / fc`. The limb
  profile reuses the ring medians of the limb-darkening fit; the edge plot keeps the profile that
  the limb PSF fit was made on (`edit::limbpsf::edge_fit`, new; `try_measure` now calls it). The
  histograms mark a plateau at the 99.5th percentile exactly as the clipped-plane check does
  (`clip`), and a plane whose top is below 30% of the range is not called clipped. The noise is
  the estimate of Immerkaer (1996), the median absolute deviation of the response to the 3 by 3
  mask (1 -2 1; -2 4 -2; 1 -2 1) divided by 6, new in `noise`, tested on white noise over a
  gradient and with 1% bright outliers. The stacked image is unchanged. Measured on the owner's
  H-alpha capture `15_32_09.avi` (best 20%, wavelets, an assumed 80 mm): the stack's noise floor
  in the power spectrum is 4.0e-7 against 4.1e-5 for the best frame, the signal to noise 269 for
  the best frame, 1406 for the stack (5.2 times, against 14 from the square root of 200 frames:
  the estimate under-reads correlated noise) and 741 after sharpening; the limb blur is sigma
  1.68 px (7.2 arcsec, misfit 1.4%); the raw red plane is 92.4% at the sensor limit, as the
  channel fallback said. Tutorial: [reading the image science
  plots](docs/book/src/tutorials/reading-the-image-science-plots.md); reference:
  [Science outputs](docs/book/src/user-guide/science-outputs.md).

- A Ko-fi support button (#202). It is in the README, the book introduction, the About chapter and the About page of the app, where **Support on Ko-fi** opens https://ko-fi.com/qisback in the browser only when clicked (the About page still makes no connection of its own). The image is the same Ko-fi button as the author's fakepve project.

### Fixed

- GitHub CI did not start
  (#235). A step name in
  `.github/workflows/ci.yml` held a colon and a space, which GitHub rejects as invalid YAML, so no
  job ran. The name is fixed, the workflows are checked with actionlint, and the push CI now tests
  Linux x86-64, Windows x86-64 and macOS arm64 only (software GPU adapter), with the Linux and
  Windows arm64 and macOS Intel targets moved to the nightly workflow.

### Changed

- Release notes are terse and impersonal (#202). `packaging/release.py notes` now writes the version, the date from the CHANGELOG heading, "What changed" with one line per change, a downloads table by platform, the source archive link and one licence line. How: each CHANGELOG bullet is cut to its first sentence, with issue links shortened to `#N` and anything after a colon or semicolon dropped, grouped Added, Changed, Fixed, Security and then any other heading. The logo stays as a small image (an HTML img 96 px wide, so it is not huge on the release page); the pre-release paragraph, the dedication and the "more from the author" line are gone from the notes; the dedication stays on the About page and in the README. The SHA256SUMS block format is unchanged, so `release.yml`, `release-windows.yml` and `release-macos.yml` read and rewrite it as before. `packaging/test_release.py` covers it and runs in the lint job.


## [0.7.0]

Science outputs and fixes since 0.6.0. `helia stack --report` writes the frame quality graphs, the shift
and drift and the seeing over the capture as PNG, CSV, JSON and one HTML page, and the Stack step of the
app has a Science panel for them. `helia stack` no longer hangs or crashes when it fails early, the
Stack step moves off a clipped colour plane as the command line does, the GPU alignment point stage
takes 1.50 s instead of 4.94 s on the largest capture, Linux arm64 is built natively, and the test suite
runs on Apple silicon. The manual gains a References page and a Credits, licences and trademarks page.

### Added

- Science outputs, group 2: alignment point plots
  (#197). `helia stack --report DIR`
  now also draws the alignment point grid on the reference frame, a heatmap of the sharpness of
  the frames each point stacks, a heatmap of how much each point moves from frame to frame, a
  vector plot of each point's mean displacement from the global shift, and (with
  `--ap-residual`) a vector plot of the residual of the shifts against the finished stack. Each
  is a PNG with its numbers in `ap-table.csv` and `ap-data.json` (the reference picture is
  `ap-reference.png`), so `helia plot ap-grid DIR` redraws it anywhere. The API job takes
  `ap_residual` (needs `multipoint`) and serves the same files; the Science section of the app
  shows the figures and has a Residual pass chip. How: the pipeline now summarises its
  per-frame, per-point table (`StackOutput::ap_stats`, `align::apstats`): for each point the
  lock rate, the mean sharpness over the frames it measured in and over its own best frames
  (the ones it stacks), the mean of its shift less the frame's global shift (the static part of
  the distortion), and the RMS scatter of that difference about the mean (the part the seeing
  changes from frame to frame). The residual is the mean and RMS of the shifts used for the
  stack less the shifts measured against the finished stack, the table behind `--ap-residual`'s
  one-line RMS. The heatmap colours span the 2nd to 98th percentile of the points well inside
  the disc, because the limb is an edge and its sharpness would swamp the interior; the vector
  scale is set so that the 90th percentile arrow is one and a half grid steps long. The stacked
  image is unchanged. Measured on the owner's 1000-frame H-alpha capture `15_32_09.avi`: 601 of
  1610 points active at 32 px every 16 px, a frame-to-frame scatter of 0.16 to 0.34 px (median
  0.27, or 1.2 arcsec) that is larger at the left limb and the bottom of the disc, a static
  displacement of 0.11 px RMS, and a residual of 0.056 px RMS; the residual pass adds 0.3 s to a
  2.5 s stack. Tutorial: [reading the alignment point
  plots](docs/book/src/tutorials/reading-the-alignment-point-plots.md); reference:
  [Science outputs](docs/book/src/user-guide/science-outputs.md).

- Science outputs, group 1: frame quality plots
  (#196). `helia stack --report DIR`
  writes the graphs astrophotographers review, AutoStakkert-style: the quality graph sorted
  and in capture order with the keep threshold, a quality histogram, the global shift and
  drift, and the seeing over the capture. Each is a PNG with its numbers beside it as CSV
  and JSON, and `report.html` is one self-contained page (figures inline, tables, the
  settings used and, for a FITS output, its header cards). `helia plot KIND DIR` redraws one
  figure from the JSON at any size; the Stack step of the app has a Science panel that shows
  them and saves the folder; an API job with `"report": true` serves them at
  `GET /jobs/{id}/report` and `/report/{file}`. How: the figures are drawn by the new
  `helia-report` crate with `plotters` (MIT; bitmap backend and `ab_glyph` only, no system
  libraries or fonts, IBM Plex Sans from the app's own assets) in a colour-blind-safe palette
  (Okabe and Ito, 2008). The keep threshold is the score of the lowest kept frame; the
  capture-order graph adds a running median (a window of a sixtieth of the frames, 5 to 301)
  and marks scores that robust ranking had put back. The drift is a least-squares line
  through the global shifts against frame number, with the RMS about it as the jitter. The
  seeing plot is new measurement: the motion index of the seeing probe, which pooled all
  frames into one number, is now also kept per frame (the squared distance of each frame's
  tile-pair differences from the pair's mean offset, averaged over the pairs, halved and
  rooted, so the RMS of the series matches the index) and smoothed over a nine-sample
  window. `--report` turns the probe on, which reads up to 120 more frames. The stacked
  image is unchanged. Measured on the owner's 1000-frame H-alpha capture `15_32_09.avi`
  (A100 for stacking, files cached): 2.05 s plain and 2.4 s with `--report`, of which 0.35 s is
  the seeing probe and under 0.1 s drawing the five figures and writing the files; the report
  shows what the numbers alone hid: the kept frames sit in two clusters (frames 50 to 250 and 300 to 480),
  the score median falls 12% over the capture (focus or air), the undriven Sun drifts
  (-51, +62) px per 1000 frames with 1.3 to 1.9 px of jitter, and the steadiest seeing
  matches the best-scoring stretch. Tutorial: [reading the frame quality
  graph](docs/book/src/tutorials/reading-the-frame-quality-graph.md); reference:
  [Science outputs](docs/book/src/user-guide/science-outputs.md).

### Documentation

- Search and sharing metadata: the documentation site has a description, Open Graph and
  Twitter card tags (a social preview image made from the logo) and a description on the main
  pages, set in `zensical.toml`, `docs/overrides/main.html` and each page's front matter. The
  workspace `Cargo.toml` gains keywords and categories, the source links (About page, packaging,
  crate metadata) point at GitHub and the Pages site instead of the private forge, and the README
  has status badges. No behaviour changed.
- **The book is checked for layout defects and the ones found are fixed.** A headless-Chromium
  checker (`.forgejo/scripts/docs-layout-check.py`, canonical copy in the CI repository) renders every
  page at 375, 768 and 1366 px in the light and navy themes, also with the navigation drawer open,
  and reports page-level sideways scroll, content clipped by `overflow: hidden`, code and tables that
  neither wrap nor scroll, oversized images, truncated or overlapping text and badly wrapping
  headings, with a screenshot of each offender. It runs in the book job as `ci / docs-layout`.
  Fixed in `docs/book/footer.css`: wide tables, including raw HTML screenshot tables, scroll inside the
  column; images, code and long words stay within it; phone headings are smaller; the header logo
  and title are sized so the title is no longer cut off on phones. Measured on the live
  manual: 22 findings on 5 pages before, none after.
- The documentation coverage page now matches the book: crop to the disc, true colour,
  limb darkening, prominences and sunspots are Done, linking their tutorials, and the
  calibration, RegiStax-style wavelet, deconvolution, colour grading and field rotation
  pages are listed against their features. Issue numbers are gone from the running text of
  the manual (the roadmap keeps its links to the tracker). `cargo xtask coverage` checks the
  links and the flag coverage as before; nothing else about the program changed.

- The testing chapter says how to run the suite on a Mac, and a dispatch-only `macos-tests`
  workflow runs it on a future Mac runner.
- Provenance and licence audit (`docs/LEGAL-AUDIT.md`). Method: every module was read for
  borrowed methods, the dependency licences were listed from `cargo metadata` and checked with
  `cargo deny`, and each cited DOI was checked against Crossref. The manual gains a References page
  and a Credits, licences and trademarks page, 24 modules gain a `# References` doc section (Richardson,
  Lucy and Dey et al. for deconvolution, Kuglin and Hines for phase correlation, Starck and Murtagh
  for the a-trous wavelets, Meeus, Howard and Snodgrass for the Sun's geometry and rotation, and so on),
  and the pages that name RegiStax, AutoStakkert! and ImPPG say that Helia is not affiliated with them.
  No behaviour changed; no code from another project was found.
- The manual is built with Zensical (the successor to Material for MkDocs) and published to
  GitHub Pages at qisback.github.io/helia, replacing mdBook. The page sources stay in
  `docs/book/src`, so the generated CLI and API references and `cargo xtask docs` are
  unchanged. The navigation moved from `SUMMARY.md` to `zensical.toml`; the toolchain is
  pinned in `docs/requirements.txt`; CI builds with `--strict`, which fails on a broken link,
  image or include (the job keeps the name `ci / mdbook`). `cargo xtask coverage` also checks
  that every page is in the navigation or deliberately excluded. The site has search, a light
  and dark theme with the logo, a link to 12hobbies.studio, tables and code that scroll instead
  of overflowing, and a Licence and terms page; the developer pages that name build machinery
  or the contribution process are not on it. README and CONTRIBUTING point at GitHub, and
  outside pull requests are not accepted until a contributor agreement is decided.
- The documentation coverage page now matches the book: crop to the disc, true colour,
  limb darkening, prominences and sunspots are Done, linking their tutorials, and the
  calibration, RegiStax-style wavelet, deconvolution, colour grading and field rotation
  pages are listed against their features. Issue numbers are gone from the running text of
  the manual (the roadmap keeps its links to the tracker). `cargo xtask coverage` checks the
  links and the flag coverage as before; nothing else about the program changed.

### Fixed

- Helia now runs its whole test suite natively on Apple silicon
  (#201). First run on a real Mac (Apple M1,
  macOS 26, Metal): the GPU parity suites (27 tests, shaders compiled by Metal) all agree with
  the CPU, as do the real-data tests and the GUI snapshots. Five `helia-gpu` unit tests failed
  there only because they insisted on a software adapter, which Metal does not have. Method: in
  a `HELIA_GPU_TESTS=hardware` run the device-store tests use the hardware device when no
  software one opens (they drive the store with a simulated limit, so the adapter does not
  matter) and the noise-floor test goes on to its hardware pass. Linux and Windows behave as before.
- `helia stack` no longer hangs or crashes when it fails early
  (#185). CLI integration tests hung
  intermittently on a loaded machine, three times. Cause, found by scanning the
  stack of a hung child: `helia stack` starts the GPU on a second thread while the capture is
  opened, and an error on the way (a bad option, a file that will not open) returned from `run`
  and called `exit` with that thread still inside the Vulkan driver's instance creation.
  `exit` runs the NVIDIA libraries' exit handlers, which wait for locks the starting thread
  holds, so the process sat on two futexes for good; other runs died with SIGSEGV or SIGABRT
  in the same window. The start-up thread is now always finished before the process can go away
  (the error path waits a few tenths of a second for the device, then releases it in order).
  Measured on a 24-core build server with an NVIDIA A100 under CPU load, 60 runs of a `helia stack` that fails on an option: 2 hung and
  51 crashed before, 0 of 300 after. The CLI integration tests now also kill a child that runs
  longer than five minutes and fail naming the test with the child's stderr, so a hang costs
  minutes, not an hour, and the `helia serve` tests print the server's stderr when it never
  reports its address.
- The Stack step of the app moves off a clipped colour plane, as `helia stack` does
  (#184, the GUI half of
  #119). On the owner's H-alpha captures
  (red 92 to 100% at the sensor limit) the app stacked the flat red plane unless the user chose
  Green or Lum by hand. How: when a capture opens and the Channel chip has not been touched, a
  raw colour capture is probed on six frames, and a video on one frame decoded as each of red,
  green and blue before it is decoded for use (the plane is taken out by ffmpeg), with the same
  `helia_core::clip` test as the command line: a plane with at least 20% of its lit pixels on
  its own ceiling is clipped, an empty plane is never chosen, and the brightest unclipped plane
  is used for scoring, previews and the stack. The Stack step says which plane and why, before
  and after stacking ("Channel: the red plane is clipped ..."), and the stack job also carries
  the fallback (`clip_fallback`), so a filter chosen later is covered too. A channel picked on
  the chip is never overridden, and the choice resets when another file is opened. Measured on
  `15_32_09.avi` (a real clipped H-alpha capture): the app opens on Green with the notice; a
  kittest checks it. The time-lapse tutorial no longer tells app users to pick Green or Lum.

## [0.6.0]

### Changed

- Band presets re-tuned on correctly labelled real captures
  (#121). The 2022-07-17
  "sodium" captures are H-alpha (the owner says so of `15_32_09.avi`; the rest show
  the same prominence and filaments): docs and tests that called them sodium are
  corrected, and the Na preset's claim of a measured gain is withdrawn (no real
  sodium capture exists yet). H-alpha wavelet gains are now 1.20, 1.60, 1.50, 1.10
  (were 1.50, 1.45, 1.25, 1.05) and Ca K 1.15, 1.50, 1.40, 1.10 (were 1.30, 1.40,
  1.25, 1.05); on four H-alpha captures this is 14 points less pixel noise and 10
  points more fibril-scale contrast. Frames kept, alignment point windows and noise
  cuts were measured and stay. The gains were raised again by the RegiStax-6-class wavelets entry below, which are the current defaults.

### Fixed

- A clipped colour plane is no longer stacked
  (#119): when the plane a
  preset names is mostly flat at the sensor limit (the red of every real H-alpha
  capture so far, the red of a Seestar video) Helia stacks the brightest unclipped
  plane and says so; `--channel` is never overridden.
- A colour AVI with no SharpCap sidecar is checked for a Bayer mosaic
  (#120) instead of being
  read as mono.
- Burned-in text detection only looks in the top and bottom strip of the frame
  (#118); it used to flatten
  parts of the disc.
- Flaky `helia-gpu` parity suite (#173).
  The suite died now and then with SIGSEGV before any test reported (about 3 runs in 50 on the
  software adapter with 24 test threads, none on the A100). A stack trace taken with a small
  `LD_PRELOAD` handler put the fault inside the Vulkan loader's `vkSetDebugUtilsObjectNameEXT`,
  called from wgpu when it names a new buffer. Debug builds turn on wgpu's debug names by default,
  and the loader is not safe against several threads naming objects at once, so the test threads
  sharing one device raced. Tolerances, test order and the shared device were not at fault: no
  numeric failure was ever seen. The instance now leaves debug names and validation off unless
  `HELIA_GPU_DEBUG` is set. Measured on a 24-core build server with an NVIDIA A100: 2 or 3 crashes in 50 runs before, 0 in 100 after
  (software), and 0 in 100 on the A100 (`HELIA_GPU_DEVICE=2`).
- Frames carrying an artefact no longer rank first or shift the stack
  (#172). A satellite trail, a dark
  line or hot pixels add sharp edges, so the damaged frame scored higher and became, or joined,
  the alignment reference: on `15_20_57.ser` with a line in two frames and hot pixels in two
  others, ranked on the whole frame, four of the five reference frames were damaged and the
  stack moved 0.14 px (the reference 0.14 px). Two changes, both in the shared pipeline so the
  CPU and the GPU behave alike. First, `quality::robust_scores` compares each frame's score
  with the median of the five frames either side of it; a score more than four times the
  capture's robust spread (1.4826 times the median absolute deviation of the log ratios) and at
  least 8% above that level gets the neighbours' level instead, and is listed in
  `StackOutput::score_outliers` and on a `ranking:` line of the command-line summary
  (`StackConfig::robust_ranking`, on by default). Second, the reference is the per-pixel median
  of the aligned top frames instead of their mean (`build_reference`), so damage in one or two
  of the five does not reach it. Damage that raises a score by less than the seeing varies, such
  as the hot pixels here (+3 to 4%), is left to the median. Measured on the same capture: the
  two frames with the line are put back (about 20% and 18% over their neighbours), the reference
  moves 0.012 px and the stack 0.011 to 0.013 px (whole frame and alignment points), from 0.14
  px. On five clean captures (`15_20_57.ser`, `Sun_Test.ser`, `15_32_09.avi` and two owner
  H-alpha captures, first 100 to 240 frames, defaults) the ranking is unchanged, nothing is put
  back, and the granulation (1 to 4 px) and fibril-scale (4 to 12 px) band contrasts are equal
  to four digits; the finest band moves by at most 0.9% (0.02054 to 0.02039, 0.01788 to
  0.01805), the noise of a different reference. Tile-median scoring and excluding saturated
  pixels were left out: they need a new device score kernel, and the
  neighbour test plus the median reference already bring the shift down to a hundredth of a
  pixel.
- release-macos builds the bundle with `build-app.sh` from `main` as well as `release.py`, so a release tag cut
  before a packaging fix (Valerian v0.2.0 still had `install -D`, which macOS lacks) can be built on the Mac.
- release-macos on the new Mac runner: the attach step ran the `release.py` of the tag being
  released, and tags cut before the notes-based checksums change download the `SHA256SUMS`
  asset, which the CI token cannot fetch (404). The workflow now takes `release.py` from `main`
  with a sparse checkout, and `attach` refuses to run when the notes hold no checksum block
  rather than overwrite the existing sums. Found by running each step by hand on the Mac.
- Robust stacking now removes artefacts that sit in one frame of many
  (#150). On a real
  capture (20 frames of `15_20_57.ser`, a 3 px dark line written into two frames
  and 300 hot pixels into two others, in sensor coordinates before alignment)
  none of `--robust sigma|huber|winsor` took out the line: the dip along it was
  -0.106 of full scale with the plain mean and with every robust mode. Two
  causes. First, the spread the rejection used was weighted by the frame quality
  weights, and the artefacts raise a frame's gradient score, so the damaged
  frames got the top weights (about 30% of the total between two frames); the
  weighted mean moved towards them and the weighted spread grew with them, which
  left a 4 sigma sample inside 1.3 sigma. Second, in the alignment point stack
  the whole-frame image that fills where no point has a tile (609 of 2135 points
  were active) was always a plain weighted mean. How it is fixed: the centre and
  spread that judge a sample are now unweighted (the weights only say how much a
  surviving sample counts), and each sample is judged against the mean and
  spread of the *other* samples at its pixel (leave-one-out, from six samples
  up; three to five use the plain spread, under three nothing is rejected), so a
  spike cannot inflate the spread it is measured against. The fill image is
  stacked with the same rule on the CPU and the GPU. Measured on the capture
  above: the line dip falls from -0.106 to 0.000 with `sigma` and to -0.045 and
  -0.053 with `huber` and `winsor` (the soft forms only pull an outlier part of
  the way back); with `--no-multipoint` -0.090 to 0.000. Clean data costs about
  the same as before (against the weighted mean, level 0.001% and contrast
  0.069% for sigma). On the synthetic known-answer test the soft forms improve
  (1% hot pixels, RMSE times 1000: Huber 3.28 to 2.85, winsor 4.71 to 3.99;
  sigma stays 1.49). CPU and GPU agree to 6e-4 at worst on the real frames.
  Regression tests: `crates/helia-gpu/tests/robust_real.rs` (real frames,
  injection in raw coordinates, whole frame and points, CPU and GPU, parity),
  plus unit tests in `stack.rs` and `pipeline.rs`. What robust stacking does not
  do is now stated by the tool and the docs: a defect fixed on the sensor is in
  the same pixel of every frame and the alignment moves it by under a pixel
  (0.45 px over those 20 frames), so it is not an outlier and `--robust` leaves
  it (measured: identical error with and without); use `--dark` or
  `--bad-pixels`. `helia stack` prints this as a note when `--robust` is on.
- Burned-in text detection only accepts text where text is
  (#118). A candidate now has
  to lie in the top or bottom strip of the frame (a sixteenth of the height, at least
  32 rows) and be at most a twentieth of the height tall (at least 24 rows). Before,
  any small flipping region in the top or bottom quarter counted: on
  `14_42_57.avi`, which has no text, eleven rectangles were flattened, one of them
  315 x 128 px inside the disc. Now none are, and the real stamps (196 x 20 px at
  the top left of the 720 x 540 captures) are still found. See
  `docs/book/src/user-guide/how-stacking-works.md` (Keeping alignment off text and
  sky).
- Five more frame-ranking metrics and a bake-off of all seven
  (#21). `--metric` (API
  `metric`) now takes `helmli`, `mfgs`, `dct`, `fourier` and `contrast` besides
  `gradient` and `laplacian`. `helmli` is Helmli and Scherer's mean (the mean of
  the pixel-to-5x5-neighbourhood ratio, inverted below 1), `mfgs` the
  median-filter gradient similarity (Scharr gradient of the frame against that of
  its 3x3 median, scored as one minus the similarity), `dct` the share of 8x8 DCT
  energy at `u + v >= 4` (after Popowicz et al. 2017 and Deng et al. 2015),
  `fourier` the power between 1/16 and 1/4 cycle per pixel in 128 px tiles less
  the white noise measured between 0.4 and 0.5, over the DC power, and
  `contrast` the mean 5x5 local RMS contrast. All are scale free and score on the
  CPU; the GPU declines them (`Metric::on_gpu`) and the pipeline falls back to
  the CPU code, so a stack with one still uses the GPU for everything else. On
  the real captures (the 2022 and 2026-10-20 sets, stacks measured with
  `helia measure` in the 1 to 4 px and 4 to 12 px bands over the same crop) no
  metric beats gradient energy on H-alpha or Ca K: all are within 6 per cent in
  the 4 to 12 px band, with no consistent winner. On the one broad-band capture
  Helmli and MFGS stack to 27 and 35 per cent more granulation-band contrast and
  48 and 63 per cent more in the 4 to 12 px band, with the same pixel noise; one
  capture is a lead, so the defaults are unchanged, and the seeing estimate (#49)
  did not predict where the metrics differ, so the default is not switched on it.
  Scoring costs relative to gradient energy on 630 frames of 1080p: `dct` and
  `laplacian` about 1x, `helmli` 1.2x, `fourier` 1.5x, `contrast` 3x, `mfgs` 5x.
  `tools/metric_bakeoff.py` repeats the comparison on any capture. New tutorial
  [Choosing a quality metric](docs/book/src/user-guide/choosing-a-quality-metric.md).
- `helia measure --centre X,Y` centres the crop on a pixel instead of on the
  centroid of the bright disc, which moves with prominences; give each stack its
  fitted disc centre to compare stacks of one capture over the same part of the
  Sun.
- `HELIA_GPU_DEVICE=N` picks the Nth real GPU adapter instead of the first, for
  machines with several GPUs where another job holds the first. The A100 server
  used for the measurements has a language model server on its first GPU, which
  had been sharing the benchmark device. `HELIA_GPU_TRACE` is documented.
- New [Performance guide](docs/book/src/user-guide/performance.md): measured
  times and peak memory on real captures from 0.4 to 5.6 GB, where the time goes,
  hardware, GPU choice and the settings that change the time.
- The GPU tone-curve kernel found the segment of a point with a loop whose
  condition read the knot table behind a short-circuit `&&`. The Windows gate, which
  runs the GPU tests on WARP, started to fail `tone_edits_match_the_cpu_reference`
  (a curve result 0.5 out) once #173 turned the instance's debug flag off, because the
  DX12 shader compiler then optimises the loop and no longer honours the short circuit.
  The kernel now takes the last interior knot at or below the value in a counted loop
  with no such condition, which gives the same segment for the ascending knots a curve
  has. The Windows gate passes again on the branch (run 10655).

### Added

- Solar differential-rotation derotation
  (#26). `helia solar-derotate
  IMAGES -o DIR --to TIME` (API `POST /solar-derotation`, `helia timelapse --derotate`,
  `derotate` in `POST /timelapse`, the Derotation section of the Time-lapse step) brings
  stacked images of the Sun to one time so that features line up across a long session or
  between panels taken minutes apart. It is not field rotation. How: each pixel of the
  result is traced back to where its feature was at the image's own time. The pixel gives a
  latitude and a longitude from the central meridian on the sphere seen at the target time
  (`B0` and `P` from the Meeus series already used for sunspots, the image orientation as
  for sunspots: `--north-angle`, `--celestial-north-up`, `--mirror`), hence a Carrington
  longitude; the feature turned in the Carrington frame (360/25.38 degrees a day) by
  `(omega(lat) - 14.18) dt` with `omega = A + B sin^2 + C sin^4`, the Howard, Harvey and
  Forgach (1990) law by default (2.894, -0.428, -0.370 microradians a second, as SunPy
  documents it) or Snodgrass and Ulrich (1990) (2.851, -0.343, -0.474) or `A,B,C` of your
  own; that gives a longitude and a pixel at the source time, sampled with Lanczos-3. The
  contrast against the fitted limb darkening is carried with the feature; the limb ring, the
  sky and what came from behind the limb are left as they were. In the time-lapse the order
  is limb shift, derotation, then phase correlation, so the correlation follows the
  pointing and not the rotation of the middle of the disc. Measured on a synthetic Sun with
  known spots (seven, at latitudes -35 to 50 degrees, drawn on a sphere with the real
  ephemeris): over six hours the disc turns 13.9 pixels at the centre and the derotated
  view matches the drawn one with every spot within 0.015 pixel and the RMS difference
  falling from 0.052 to 0.0003; over 24 hours the differential law leaves an RMS of 0.0009 where
  a rigid rotation at the equatorial rate leaves 0.0115. On the two real H-alpha captures of
  2026-10-20, five minutes apart, the phase correlation of the disc centre asks for 0.18
  pixel without the derotation and 0.35 with it: the 0.17 pixel difference agrees with the
  0.185 pixel the model predicts at the disc centre. There is no real series spanning hours
  yet, so over hours the model is checked against itself and not against the Sun. See the
  user guide's Solar derotation page and the tutorial, Derotating H-alpha stacks to one time.
- Time-lapse (#26).
  `helia timelapse CAPTURES... -o DIR -- STACK OPTIONS` (API `POST /timelapse`, the
  Time-lapse step in the app) stacks every capture with the same settings, puts the
  stacks in time order, registers them to one of them, optionally evens out the
  brightness and crops to the common area, and writes `frame-0001.png` and so on (or
  16-bit TIFF), a registration table `timelapse.csv` and a movie: an MP4 through
  `ffmpeg` when it is installed, else an animated PNG that Helia writes itself. How:
  each stack is registered in two steps. The limb fit (720 rays, a Tukey-biweight
  circle fit, the method of 0.2.0) gives the disc centre to about a tenth of a pixel
  whatever is on the disc; then FFT phase correlation of the middle of the disc (a
  square of 1.4 radii, after the limb shift, with sub-pixel parabola refinement)
  refines it by following the granulation and filaments themselves, and is added only
  when its peak-to-sidelobe ratio is at least 6 and its correction at most 3 pixels
  (`--max-refine`). A frame with no limb is correlated whole (trusted at a ratio of
  4); one with neither is left alone and reported. Frames are moved with a Lanczos-3
  kernel, colour planes alike. The brightness is the mean over 0.8 of the disc radius;
  `--deflicker match` takes every frame to the median of those means, `smooth` to a
  running median of 3 or more neighbours, as a gain only. Capture times come from SER
  stamps, a FITS `DATE-AVG`, or the file name (`15_32_09.avi`, with `--date` and
  `--utc-offset`; an AVI has no stamps). The MP4 is fed raw RGB on `ffmpeg`'s standard
  input (H.264, falling back to MPEG-4 part 2) and the animated PNG is the `png`
  crate's APNG writer. Measured on two real H-alpha captures five minutes apart
  (2026-10-20, 720 by 540, 1000 frames each): the discs were 17.2 and 5.9 pixels
  apart; the limb fits and the correlation (peak-to-sidelobe ratio 180, correction
  0.18 and 0.40 pixel) registered them, the mean absolute difference inside 0.9 of the
  radius fell from 0.064 to 0.026 and its RMS from 0.071 to 0.036 of full scale, and
  the whole run, two stacks included, took 5.5 s. On synthetic discs with known shifts
  of up to 7 pixels the recovered shifts are within 0.1 pixel and the registered
  frames agree to 1% RMS. The MP4 was checked with ffmpeg 7.0.2 (H.264, `yuv420p`).
  See the user guide's Time-lapse page and the tutorial, A time-lapse of two H-alpha
  captures.
- Prominence layer: IMPPG-style adjustment
  (#134). The layer has a join
  offset from the fitted limb, a feather, and a separate gamma for the disc and for the
  off-limb layer, all as live sliders at the top of the Adjust step, matching what
  users expect from prominence processors (IMPPG's public README was read for this: a
  tone curve applied last, brightness-adaptive amounts for the disc and the dim
  prominences, live preview; no code was used). How: the join radius is the fitted limb
  times one plus the offset; the gammas act on the tone above the sky as a power of the
  value over the quiet disc level, and are 1 by default, so the disc stays bit-identical.
  Tutorials with real captures were added for sunspots and prominences.
- Colour grading, and a more orange-red H-alpha
  (#133). A **grade** on
  top of each band's look: hue (-180 to 180 degrees), saturation (0 to 2),
  brightness gamma (0.3 to 3), black and white points and a curve for each of
  red, green and blue, with a live preview, saved as named colour presets (GUI
  Colour step; `--grade hue=...,sat=...` or a preset name; API `grade`). *How.*
  The grade is part of the colour table: black, white and gamma on the mono value
  before the look, per-channel curves after it, then a hue rotation about the
  grey axis and a saturation about the Rec. 709 luma, so CPU and GPU agree
  (parity-tested) and an exported `.cube` or `.lut` carries it; a neutral grade
  changes nothing. The H-alpha look moved from gains 1.0, 0.85, 0.60 to 1.0, 0.45,
  0.10 (exponents 0.70, 0.90, 1.40), the common H-alpha palette; mid-grey maps to
  RGB 169, 68, 11 instead of 169, 90, 33. Judged by eye on the real H-alpha
  stacks (three candidates compared), the old look was pale cream. New tutorial
  "Colour grading your band".
- Deconvolution that shows what it does
  (#132). A PSF measured
  from the solar limb (`--rl-psf limb`, GUI **Limb**, now the default; `helia
  measure --psf`), an iteration scrub and a **Then wavelets** chip in the
  Sharpen step, `--wavelets` after `--sharpen rl` (deconvolve first), and a check
  of every sharpening: detail gain (1 to 4 px band) against noise gain (pixel
  band) and, for deconvolution, the residual against the noise level, with a
  warning when the result is mostly noise (GUI, `check:` and `warning:` lines,
  API `summary.sharpen_check`). *How.* The limb profile is the median over 360
  angles; a limb-darkened step blurred by a Gaussian (erfc model) is fitted by
  least squares over edge radius and sigma, the pixel box removed in quadrature.
  *Measured* on the H-alpha stack `15_32_09.avi`: limb sigma 1.68 px against
  0.70 px from the image scale; RL with the scale PSF gives detail x0.98, noise
  x1.64 (flagged), with the limb PSF and TV 0 detail x2.04, noise x2.07. The TV
  default fell to 0, as it raised noise on every stack tried. New tutorial
  "Deconvolution".
- RegiStax-6-class wavelets
  (#131), after an
  early-access tester found the images soft against RegiStax 6. The wavelet step
  now has the controls of RegiStax 6: six layers (up to eight on the command
  line), each with an **amount** (the layer slider, 0 to 200%), a **sharpen**
  (-0.1 to 1) and a **denoise** (0 to 1.5); a **dyadic** or **linear** scheme with
  an initial layer and a step; the **default** (B3 spline) or a **Gaussian**
  filter; and **linked** layers. `--wavelets registax:l1=100/0.14/0.2,...`
  (API: `wavelets`; an `edits` entry `{"op": "wavelet", "scheme", "initial",
  "step", "filter", "linked", "layers": [{"amount", "sharpen", "denoise"}]}`;
  the older `gain` and `threshold` of a layer are still read). *How it is
  achieved.* RegiStax is closed source, so the behaviour and ranges follow its
  manual, the author's pages and published tutorials (listed in the user guide's
  "Coming from RegiStax"), not its code. The engine is the existing a-trous
  transform with a per-layer tap spacing (`2^(initial-1+j)` for dyadic,
  `initial + step*j` for linear), a selectable kernel, and an optional linked mode
  in which each layer smooths the running result instead of the previous
  smoothing. A layer is a gain `1 + amount/100 * 100 * sharpen * |sharpen|` and a
  noise cut `amount/100 * 10 * denoise` noise sigmas (soft threshold on the
  layer's own MAD noise estimate). The quadratic sharpen scale gives a 2x to 7x
  gain at the 0.1 to 0.25 that tutorials use and 10x to 100x at the top of the
  range. The GPU runs the same layers, schemes, filters and linked mode, checked
  against the CPU in the parity tests. In the app: scheme, filter, linked, initial
  layer and step, an amount slider with numeric entry, sharpen and denoise boxes
  per layer and the resulting gain; a **1:1** button; and a preview that is shrunk
  while a control is dragged and made again at full size when it is let go. User
  presets (the existing My presets) keep the whole set. *Measured effect*, on the
  owner's H-alpha capture (250 of 1,000 frames of `15_32_09.avi`, the 1 to 4 px
  band of `helia measure`): the stack 0.96% of the mean, the old recommended
  wavelets 1.22%, the new ones 2.17%, a RegiStax-tutorial set 2.05%; the
  pixel-scale band (noise) 0.30%, 0.40%, 0.62% and 0.72%. On Ca K
  (`15_44_48.avi`) the 1 to 4 px band 0.088%, old 0.064% (it lost detail), new
  0.104%, with pixel noise 0.048% against 0.115% in the stack, over one fixed
  crop of the stack's disc centre. The band presets
  were re-tuned (H-alpha gains 1.4, 3.2, 2.4, 1.6; Ca K 1.7, 2.8, 2.0, 1.4) so a
  first result is crisp; the others got a proportionate lift without data. New
  pages: "Coming from RegiStax" (the control mapping) and the "Sharpening like
  RegiStax" tutorial, with real-capture crops. A real-data test
  (`wavelet_real`) holds the lift on the H-alpha capture.
  References: Starck and Murtagh, *Astronomical Image and Data Analysis*
  (Springer, 2002) for the a-trous transform and the B3 spline; Holschneider et
  al., 1989, for the algorithm "a trous".
- Altazimuth field rotation correction from the SER timestamps
  (#24). `--field-rotation`
  (API `field_rotation`, Advanced settings in the app) turns every frame to one
  orientation, from the parallactic angle of the Sun (latitude, longitude and time;
  the site is `--site` or `HELIA_SITE`) at the frame's timestamp, before the frame
  is scored, aligned or stacked, so a long capture on an altazimuth mount no longer
  smears the limb. `helia derotate` does the same for a time-lapse of stacked
  images, from a FITS `DATE-AVG` or `--times`. The angle matches Meeus's worked
  examples and the textbook rate at noon; on a synthetic hour of rotation the error
  against the true image falls 26 times, and on the real capture `15_20_57.ser`
  (given the timestamps of an hour, 25 pixels of turn at the limb) the granulation
  band of the stack goes from 0.71% back to 1.30% (the frames as they were: 1.29%)
  on global alignment and from 0.92% to 1.32% (1.31%) with alignment points. See
  the user guide's Field rotation page.
- Sunspot detection with group count, Wolf number and corrected area
  (#57). `helia sunspots
  IMAGE` (and `helia stack --sunspots`, API `sunspots`, the Sunspots chips in the
  Stack step) flattens the limb darkening, finds the penumbra and umbra against a
  local quiet-Sun level, groups the spots by their separation on the Sun, and
  reports the number of groups and spots, the relative Wolf number `k (10 g + s)`,
  and every spot's area in millionths of the hemisphere as seen and corrected for
  foreshortening, with its heliographic position. `P`, `B0` and `L0` come from the
  capture time (SER stamps, a FITS `DATE-AVG`, or `--date`) with the series of
  Meeus, checked against his worked example. Output: a CSV table, a JSON report, an
  overlay PNG, `SPOT*` and `CRLT_OBS`/`CRLN_OBS` FITS cards, `summary.sunspots`
  and `GET /jobs/{id}/sunspots.csv` and `.png` in the API, and a Save button in the
  app. On synthetic discs with known spots the counts are exact, areas within 5%
  and positions within 0.5 degree; on a real white-light disc (2025-09-17) it finds
  7 groups and 13 to 14 spots, Wolf 83 to 84, at 6 to 50% of the frames kept. See
  the user guide's Sunspots page.
- Prominence and disc compositing, and colour table export
  (#23). `--prominence`
  (an `edits` entry `{"op": "prominence"}`, the Prominence layer in the Adjust
  step) takes the glow off outside the limb (the median of each ring in a 45
  degree window, so a glow that is brighter on one side is followed), stretches
  what is left through an `asinh` curve and joins it to the disc through a mask
  feathered about the fitted limb, so prominences and disc are both readable in
  one picture. A field with no prominences stays dark. On the two H-alpha
  captures of 2026-10-20 the brightest 1% of the ring outside the limb goes from
  0.33 and 0.25 to 0.69 and 0.71 of the disc's brightness, the glow in the ring
  from 0.087 and 0.020 to 0, and the disc inside 0.9 of the radius is
  bit-identical. `helia lut PRESET -o FILE` (API `GET /presets/{id}/lut`, the
  Colour table chips in the Colour step) writes a filter's colour look as a 1D
  `.cube`, a `.csv`, an ImageJ `.lut` or a PNG strip. See the user guide's
  Prominences page and the Exporting a colour table section of Colourising.
- True colour from a raw colour camera (#72).
  `--colour true` (API `colour: "true"`, the **True colour** chip in the Stack
  step) stacks the red, green and blue planes on one alignment (the same global
  shifts, alignment point field and frame weights, found on the luminance),
  registers red and blue to green to remove atmospheric dispersion, white balances
  the three (`--white-balance auto|off|R,G,B`, `--no-channel-align`) and writes an
  RGB TIFF, an RGB PNG or a three-plane FITS cube. Sharpening and the other edits
  apply to each channel. The mono filter presets are unchanged: they stack one
  plane and colour it from a look, as before. The result carries what was done
  in its history, in the CLI summary and in `summary.true_colour`; a channel that
  holds almost no signal (blue behind a sodium filter) is held at a gain of 4 with
  a warning rather than amplified into noise. Known-answer tests on a synthetic
  colour disc (balance, dispersion, crop) and a CPU and GPU parity test. A
  three-plane FITS file also opens in `helia edit`, and `helia edit` can write one.
- Clipped colour planes are stacked from the next plane
  (#119). A narrow-band
  filter lights one Bayer plane far more than the others, and an exposure that
  shows the leak or the prominences clips it: on every real H-alpha capture so far
  (a ZWO ASI482MC, 2022 and 2026) 92 to 100% of the lit disc pixels of the red
  plane sit at one value, so the stack is a flat white disc. Before stacking,
  `helia stack` and the API (`StackConfig::clip_fallback`) read six frames, take each
  plane's ceiling as its 99.5th percentile (so hot pixels above a plateau do not
  hide it), call a plane clipped when 20% or more of its lit pixels sit within
  rounding of that ceiling (4/255 for decoded video), and stack the brightest
  plane that is not, never an empty one. A note names what happened and how to
  avoid it (shorter exposure, lower gain). A plane named with `--channel` is never
  changed, and the app keeps its explicit channel chips. Measured: the
  granulation-band contrast of `15_37_19.avi` is 0.14% from red and 1.39% from
  green. See the FAQ entry on a clipped red channel
  (`docs/book/src/user-guide/faq.md`).
- A colour AVI with no SharpCap sidecar is checked for a Bayer mosaic
  (#120). An AVI does not
  say whether it holds raw colour, so a capture copied without its
  `.CameraSettings.txt` was read as mono and carried its mosaic into the stack. When
  the sidecar names no pattern, the pixels get the same test as a SER file labelled
  mono (`io::detect`: the four pixel phases of a 2 x 2 cell on the disc differ in
  mean by 5% or more). The sidecar still wins when there is one, and a dim target
  (mean disc level under 5% of full scale) is never guessed at, so a planetary AVI
  from a mono camera stays mono. On the owner's captures `15_37_19.avi` and
  `15_36_42.avi` are found as RGGB and `15_44_48.avi` (a mono camera) stays mono.
  See `docs/book/src/user-guide/troubleshooting.md` (a grid or chessboard in the
  stack) and `docs/book/src/recipes/fixing-a-bayer-grid.md`.
- Seeing estimation from the capture
  (#49). `--seeing` (API
  `seeing`) follows tiles of the disc through 120 frames spread over the capture and
  gives the Fried parameter `r0` and `D / r0` from the spectral ratio (von der
  Luehe's method, with the telescope aperture) and a relative motion index from how
  much neighbouring alignment points move against each other. Printed after the
  stack, in the FITS header (`SEE*` cards), in `summary.seeing`, and as a line in
  the Stack step of the app (the aperture is under Advanced settings). On
  synthetic Kolmogorov bursts `D / r0` from 3 to 12 is recovered to within 5%,
  1%, 9% and 19% and does not depend on the object; the motion index follows the
  theory's 5/6 power. On the real captures the best frames read as better seeing
  than the worst on all three tested; the absolute `r0` is only meaningful at about
  0.2 arcsec per pixel or finer and Helia says so. See the user guide's Seeing
  estimation page (`docs/book/src/user-guide/seeing.md`).
- Mosaics from panel stacks
  (#26). `helia mosaic PANELS -o FILE`
  (API `POST /mosaic`, the Mosaic step in the app) joins panels of the Sun into a full-disc
  picture: stacks the captures the same way (or takes stacked images), matches every pair on its
  overlap, places all the panels at once, matches their brightness, joins them along seams of
  least difference, feathers the seams and optionally derotates the panels to one time first.
  Writes the mosaic, `NAME-panels.csv` (where each panel went, its turn, gain and offset) and
  `NAME-pairs.csv` (how each pair matched). How: each panel becomes a picture of its detail (a
  difference of Gaussians over its local size, so gain and background do not matter). A pair is
  found by the masked normalised cross-correlation of Padfield (2012), by FFTs, over trial turns
  every 0.75 degree on pictures shrunk to about 128 pixels, keeping the best six peaks; each is
  refined by a Gauss-Newton (Lucas-Kanade) fit of shift and turn on a pyramid from a factor of 8 to
  full size, and the candidate that agrees best at full size is the match. The panels are placed
  by weighted least squares, the turns first and then the shifts (each pair puts the centre of
  one panel in the other), with a Huber reweighting, putting aside and naming a pair that stays
  more than 3 pixels (or four times the median) off. Brightness: one least squares fit for every
  gain and offset at once over all the overlap pixels, panel 1 held. Seams: for each panel added,
  the path across its overlap with what is there that crosses the smallest differences (dynamic
  programming), each side owned by one panel, the ownership masks blurred by the feather. With
  `--derotate` the disc is fitted to a first joining, each panel is derotated to one time with it
  and the pairs are matched again. Measured on six panels cut from the real stack of
  `15_37_19.avi` with known offsets (to a tenth of a pixel), turns (up to 1.1 degrees), gains
  (0.91 to 1.07) and offsets, with noise at the stack's level: worst placement error 0.031
  pixel, worst turn error 0.006 degree, gains to 0.3%, RMS against the stack 0.0036 of full scale
  (the panels' noise is 0.004; 0.00036 without noise, with the seams 0.13% of the level); without
  the brightness match the RMS is 0.023. On four crops of the stack placed on whole pixels, 0.02
  pixel. A synthetic rotating Sun with panels four hours apart: RMS error 0.0011 with the
  derotation, 0.018 without. **There is no real mosaic capture yet**, so panels that differ in
  seeing, flat-field and camera rotation are untested. See the user guide's Mosaics page and the
  tutorial, Joining panels into a full-disc mosaic.

### Performance

- GPU: the alignment point stage reads back a few floats per point, not the
  correlation surfaces and the noise-floor samples
  (#10, the next step
  found by [#169](https://github.com/qisback/helia/pull/169)). A batch used to read 9.1 MB
  back on the 5.6 GB capture (1.7 MB of surfaces, 7.4 MB of samples) and took
  7.7 ms with every kernel skipped. How: a new kernel (`floor_select.wgsl`)
  finds each frame's noise floor, the 1st percentile of the finite samples, by
  radix selection (four rounds of a byte histogram over an order-preserving key,
  32 workgroups per frame, then one thread picks the byte holding the rank), so
  the answer is a sample and does not depend on summation order; and the
  correlation kernel applies the correlation threshold and the border test
  itself and returns 14 floats per point (offset, peak, window sums, the 3x3
  neighbourhood of the peak). The sub-pixel fit (`fit_patch`, split out of
  `fit_peak`) and everything after it run on the host as before. The fit was
  also tried on the device and dropped: 80 to 93 per cent of the fits differed
  from the host's in the last digits, by up to 1.5e-5 px (fused multiply-adds and
  the device's `log`), more than the 1e-6 allowed and enough to change the
  stack. The batch size is unchanged (the freed buffers are still counted when
  it is chosen), because a different grouping of frames changes the stack in the
  last digits. Measured on the A100 (`HELIA_GPU_DEVICE=2`, interleaved, best of
  three, re-measured): the 5.6 GB capture's points stage 4.94 to
  1.50 s and whole run 12.06 to 9.19 s; the stage more than halves on the other
  captures (0.14 to 0.64 s after), the whole run gains 0.1 to 1.1 s; video memory +25 MB, RSS unchanged. The TIFF
  checksums match on all seven captures and, on the three smaller ones, on the
  software adapter, where it makes no measurable difference (the stage 3.24 to
  3.01, 6.12 to 6.18, 9.43 to 9.07 s). WARP: the first landing (#181) failed three
  parity tests on the Windows lane's software DX12 adapter, the device floor
  coming back as -0.5 where the host had -0.96. The cause is the DX12 shader
  compiler (WARP) miscompiling the bin search in `pick`: a loop with a
  data-dependent `break` whose counter is read after the loop. After the first
  radix round it fell off the end of the loop (bin 255) instead of breaking, so
  every later byte was 0xff and the floor was the key 0x40ffffff, which is -0.5.
  Lavapipe and the A100 compiled it correctly. The search now scans all 256 bins
  with no break and no short-circuit (the same cure as for the tone-curve kernel
  in the fix above), and the bytes already chosen are masked from a table, not a
  shifted constant. Confirmed on the runner: the new mask with the old loop
  still fails the same three tests; with the new scan the Windows lane passes.
  Tested: the floor kernel against the host
  selection on ties, infinities, NaN, negative zero, wide ranges and frames with
  no finite sample (software and hardware adapters, run twice to show the
  records clear), and a parity test over the rejection paths (peaks on the border,
  correlation under the threshold, search radii 1, 2, 4 and 7).

- Profiled the whole of `helia stack` on the real captures (stage times by
  `--timings` and `HELIA_GPU_TRACE`; 24-core CPU, NVIDIA A100 over Vulkan and the
  lavapipe software adapter) for
  #10. Result for the
  5.6 GB, 2713-frame 1080p Ca K capture: 27.6 s on the CPU (alignment points 17.8
  s, 2.2 GB peak) and 13.8 s on the GPU (alignment points 5.4 s, 0.64 GB peak);
  the 630-frame 1080p capture that #10 measured at 85 s on the laptop takes 4.1 s
  on the CPU and 3.5 s on the GPU. The targets of #10 (10 s on the GPU path for
  that capture, three times faster on the CPU) were already met by the earlier
  work; what remained was scheduling.
- GPU alignment points: the window sums of the interpolated points are queued
  without waiting and collected one batch later with a wait on that submission
  alone (`map_f32_after`), where they used to wait for the whole queue, which
  included the batch queued behind them, so the device idled while the host fitted
  peaks. The host's wait for them fell from 3.8 s to 0.04 s on the 5.6 GB
  capture. Output is identical (stacked FITS files equal byte for byte but the
  creation date).
- `helia stack` starts the GPU device on a second thread while it opens and
  probes the capture, hiding about 0.2 to 0.4 s of start-up.
- Measured effect of the two changes, alternating builds on one machine, best of
  four: 12.27 to 11.86 s (5.6 GB capture), 3.58 to 3.21 s (630-frame 1080p),
  2.31 to 2.07 s (1000-frame 720x540), 3.45 to 3.15 s (1936x1088 mono); peak RSS
  unchanged (about 0.45 to 0.7 GB on the GPU path). Tried and not kept: copying
  uploaded frames into device staging memory with several threads (no change,
  memory bandwidth bound) and an AVX2 build (3 to 7 per cent on the CPU path, not
  portable).
- Not done in that round: keeping frames on the device between stages (done
  next, see below) and coarse-to-fine alignment point correlation on the CPU
  (about 16 times fewer operations, but it changes the output beyond 1e-6). Both
  were recorded in the Performance guide as the next steps.
- AutoStakkert!: its author publishes no timings, so there is no figure to match;
  the guide cites its change notes and says so.
- GPU: raw frames stay on the device between stages
  (#167, part of
  #10). Scoring, global
  alignment, alignment points and stacking each used to read their frames from the
  file and upload them again (15.8 GB of reads and uploads for the 5.6 GB capture).
  How: the first stage to need a frame uploads it as before and also copies the
  stored bytes, device to device, into a store of 128 MiB chunk buffers
  (`crates/helia-gpu/src/resident.rs`); later stages find the slot, copy the frame
  from the store into their batch's raw buffer, and the same decode kernel runs on
  the same bytes, so the output cannot differ (and does not: the TIFF checksums
  match on all seven real captures, at store sizes from 0 to 6 GiB, on the A100
  and on lavapipe). The store belongs to the source (`SharedFrames::device_cache`,
  a `DeviceCache` the file, limited, calibrated and masked sources carry), so it
  dies with the capture, and the pipeline calls `Backend::release_frames` when a
  run or a scoring pass ends. It is bounded: `HELIA_GPU_RESIDENT_MB`, default
  6 GiB on a discrete adapter and 0 on software and integrated ones (where the
  upload is a memory copy and the store measured no faster and 0.3 to 0.9 GB more
  RSS). wgpu does not report video memory, so the device is the guard: when it
  reports out-of-memory the store stops growing, gives back 1.5 GiB for the
  stages' buffers and keeps what it has, and frames that did not fit are read from
  the file. Each stage names the frames it will use and the store swaps out frames
  outside that set for frames inside it. Measured on the A100 (`HELIA_GPU_DEVICE=2`,
  interleaved, best of three): global alignment of the 5.6 GB capture 1.56 to
  1.22 s and stacking 1.04 to 0.98 s, but scoring (which must upload every frame
  once, at about 2.3 GB/s into staging memory) 3.42 to 3.50 s and alignment points
  (device and read-back bound: 12 MB of correlation surfaces a batch) 3.91 to
  3.81 s; whole runs 11.37 to 11.06 s, within the noise of a shared host, on all
  seven captures (-4 to +4 per cent). Peak video memory rose from 0.5 to 1.4 GiB
  to 1.0 to 6.2 GiB, peak RSS did not change (326 to 777 MB). The honest summary
  is that the repeated uploads were already hidden behind the device's work in the
  stages that matter, so this saves reads (and wins where the bus is slower or the
  file is not cached) rather than seconds on this hardware. Tested: the same
  answers from scoring, registration, alignment points and the stack at store
  sizes of 0, 1, 3, 12, 40 frames and the default (new parity test); the store's
  eviction, rollback and out-of-memory paths (unit tests, and a test that asks the
  A100 for a 4 TiB store, which it refuses at 78 GB while the store keeps 76 GB and
  carries on). Profile findings that follow from it, in the guide's next steps:
  the alignment point stage is bound by the device and by the read-back, and
  larger batches (4.5 to 3.3 s) change the stack in the last digits.

### Changed

- The aarch64 Linux downloads (tarball, `.deb`, `.rpm`, AppImage) are now built and
  tested natively on a 64-bit ARM runner (an Ampere arm64 server)
  instead of being cross-compiled on x86-64 and checked under qemu
  (issue #159). How: `packaging/linux/native-arm64-build.sh` builds the release
  binaries inside an `ubuntu:22.04` arm64 container, which keeps the glibc 2.35 baseline,
  and re-runs them in that image; the packages are assembled with the aarch64
  appimagetool (pinned by SHA-256) and checked by running the binaries from the `.deb`,
  `.rpm` and AppImage, then the `.deb` is installed, run and removed in a clean
  `debian:bookworm` arm64 container. CI gains an arm64 Linux job (clippy and
  `test --workspace` with the software GPU), not yet a required check. Measured on the forge: the whole aarch64 job takes 4m26s (Helia, dry run) and 3m49s (Valerian) against 9m00s and 8m40s for the cross-built jobs of the 0.5.0 and 0.2.0 releases; the newest glibc symbol is 2.35.

## [0.5.0]

Fifth pre-release. Helia now installs like a desktop application on Windows and Linux
(#73): a Windows setup program, a
`.deb`, a `.rpm` and an AppImage. It also gains limb-darkening fitting, automatic alignment points and a disc crop.

### Added

- Limb-darkening fit and flatten (#20). Every stack now
  has its limb darkening fitted from the azimuthal median of the disc, with the
  linear, quadratic (the default) or five-term Neckel and Labs law
  (`--limb-law`, API `limb_law`); the coefficients are printed (`limb:` line),
  returned in `summary.limb_darkening` and recorded in a FITS `PRSTEP` card.
  `helia measure --limb` fits all three laws to any image and prints the profile.
  `--flatten model|profile` (API `flatten`, the Limb darkening chips in the Sharpen
  step, or an `{"op": "flatten"}` edit) divides the disc by the fitted law or by
  the measured profile before anything else, so sharpening sees flat contrast.
  Only the disc changes, the gain eases to 1 at the limb, and it is scaled so the
  disc's bright end does not run past white. The edit has a GPU kernel and a parity
  test. On the project's sodium captures the linear coefficient is 0.50 to 0.56
  against 0.56 in the literature at 600 nm; the details are on the Sharpening
  page (`docs/book/src/user-guide/sharpening.md`, Limb darkening) and the Sharpen
  walkthrough step (`docs/book/src/walkthrough/06-sharpen.md`).
- `helia_gpu::GpuContext::device()` and `queue()`, and `helia_gpu::wgpu` (a
  re-export of the wgpu it is built on), so another program can write its own
  kernels on the device Helia opened, with Helia's adapter choice and teardown
  handling. Valerian uses them for its GPU kernels
  (valerian#4).
- Automatic alignment points and a disc crop
  (#25). `--ap-auto` (API
  `ap_auto`, the **Auto** chip in the Align step) fits the limb for the image
  scale, probes the seeing on a dozen frames and picks the alignment point
  size, spacing and search range, and says what it chose. `--autocrop` (API
  `autocrop`, the **Crop: Disc** chip in the Stack step, with `--autocrop-margin`
  as a percentage of the radius, 10 by default) cuts the result, FITS included, to
  a square around the fitted disc. A capture with no clear disc is left whole,
  with a note. Both are off by default. On the project's four sodium captures the
  automatic size is 32 px on all of them (the same as the photospheric preset),
  the search range 4 to 7 px, and the granulation band is within 0.1% of the
  fixed 32 px run; the measurements are in
  the user guide's How stacking works page
  (`docs/book/src/user-guide/how-stacking-works.md`), and the Align and Stack
  walkthrough steps show the chips (`docs/book/src/walkthrough/04-align.md`,
  `05-stack.md`).
- A Windows installer, `helia-<version>-x86_64-pc-windows-msvc-setup.exe` (NSIS).
  It installs for the current user with no administrator rights, or for all
  users, adds a Start menu entry, an optional desktop shortcut and an optional
  PATH entry for `helia`, lists Helia under Open with for `.ser`, `.avi` and
  `.fits` files without becoming their default program, shows the GPL and the
  additional terms, and uninstalls cleanly from Apps and Features. It takes
  silent switches (`/S /AllUsers /ADDPATH /DESKTOP /NOSTART /NOASSOC /D=`). It is
  not code-signed yet (#70),
  so SmartScreen warns on first run; the install guide says what to do. The
  release build installs it silently on the Windows runner, checks it and
  uninstalls it again. See `docs/book/src/user-guide/installation.md`.
- A `.deb` (amd64, arm64), a `.rpm` (x86_64, aarch64) and an AppImage (x86_64,
  aarch64) for Linux, with the programs in `/usr/bin`, a menu entry, icons in
  eight sizes, AppStream metainfo, and a MIME type for SER files. The release
  checks the desktop file and metainfo with `desktop-file-validate` and
  `appstreamcli`, and installs and removes the `.deb` in a clean container. See
  `docs/book/src/user-guide/installation.md`.
- `Helia.app` and a `.dmg` packaging recipe, and a `release-macos.yml` workflow
  that only runs on request on a macOS arm64 runner. Nothing is
  published for macOS yet: there is no Mac build machine
  (#75).
- The desktop app remembers the size of its window and, under **Recent** in the
  top bar, the last eight captures you opened. `helia-gui capture.ser` opens a
  capture, which is how Open with works.
- An Installation chapter for Windows, Debian and Ubuntu, Fedora, AppImage and
  macOS (coming), with uninstall steps (`docs/book/src/user-guide/installation.md`).
- Two-pass alignment point refinement and shift field smoothing
  (#4). `--ap-passes 2`
  (API `ap_passes`) stacks once and measures the alignment points again against
  that stack; `--ap-smooth on` (API `ap_smooth`) replaces shifts that disagree
  with their neighbours; `--ap-residual` prints how far the shifts used are from
  a measurement against the finished stack. All off by default. On the four
  sodium captures the second pass cut that residual by 1.7 to 3.6 times and
  raised the granulation band by 1.8% (`Sun_Test.ser`) and 1.2%
  (`14_42_57.avi`); smoothing changed it by under 0.1%. On synthetic data the
  second pass did not reduce the error against the truth. Numbers and the
  trade-offs are in the user guide's How stacking works page
  (`docs/book/src/user-guide/how-stacking-works.md`, Refining the shifts).
- Granulation statistics as a quality report
  (#56). For white-light and
  sodium stacks `helia stack` prints, and the FITS header (`GRAN*` cards), the API
  (`summary.granulation`) and the Stack step carry, the RMS contrast of the
  flattened disc centre, the mean granule spacing and diameter, and the radially
  averaged power spectrum's roll-off and resolved frequencies, with the diffraction
  cut-off when the aperture is given (`--aperture-mm`, API `aperture_mm`).
  `helia measure` prints the same for any image (`--scale`, `--aperture`,
  `--wavelength`). On the four sodium captures the contrast ranks a selected
  12% with alignment points above global alignment and above all frames on all four.
  The cell size is only meaningful at about 0.5 arcsec per pixel or finer, and the
  report says so. See the user guide's granulation and seeing report page
  (`docs/book/src/user-guide/quality-report.md`).
- Self-calibrated flats from displaced-disc frames
  (#52).
  `helia calibrate --displaced <captures>` (and `helia stack --displaced`, API
  `calibration.displaced`) solves the sensor's gain and the Sun together from frames
  with the disc at different places on the sensor (Kuhn, Lin and Loranz 1991),
  giving a flat with the dust and fringes of the observing moment without a
  diffuser. Displacements come from the limb fit, a colour mosaic is solved plane by
  plane, and the smooth scales the data cannot separate from the Sun are removed
  (`--displaced-keep-smooth` keeps them). On synthetic frames the gain is recovered
  to 0.54%, 0.11% and 0.08% RMS from 10, 25 and 50 frames, and a displacement of
  under 30 px is refused. On the project's tracked captures a flat from their own
  drift did not help (it took granulation with it at 16 px of drift), so the real
  displaced captures of the issue are still wanted. See the user guide's
  calibration page (`docs/book/src/user-guide/calibration.md`, A flat from the Sun
  itself).

### Changed

- `helia-gui.exe` is a Windows GUI program: double-clicking it no longer opens a
  console window. `--version` still prints when run from a terminal.
- `SHA256SUMS` now lists the Windows files as well as the Linux ones, and the new
  packages. The Windows build adds its lines to the file the Linux build wrote.
- The Linux desktop file opens several files (`%F`), lists the SER, AVI and FITS
  types and has the trailing semicolons the specification asks for.

## [0.4.0]

Fourth pre-release. The photospheric presets now keep granulation (#101), the stack can be made robust against outliers, frames can be calibrated with darks, flats and a bad-pixel map, and the tone curve can be dragged.

### Fixed

- The real-data stack tests no longer fail on `main`
  (#62). They measured the
  stack against the best raw frame, but the pipeline has flattened burned-in text
  since [#34](https://github.com/qisback/helia/pull/34); the text edges
  counted on one side only. Both sides now blank the masked rectangles.

### Changed

- The photospheric presets (white light and Na D) keep granulation
  (#101): they keep the
  best 12% of frames (was 25%), size alignment points at 2.5 granules (32 px at
  full-HD scale, or from the image scale when it is known), lift wavelet layers 2
  and 3 with no noise threshold there (the finest layer keeps its threshold), and
  Na D stacks the green channel instead of the luminance, because the sodium
  filter leaves the blue pixels empty and averaging them in printed the mosaic over
  the granulation. On the project's sodium captures the granulation band measured
  1.6 to 2.8 times higher. The chromospheric presets are unchanged. See
  `docs/book/src/recipes/white-light-granulation.md` and the Colour presets
  reference (`docs/book/src/reference/presets-table.md`).
- The feedback warning no longer says the issue is public, because the project
  repository is private for now: "Your feedback is sent to the Helia developers
  and filed in the Helia issue tracker. Check the screenshot shows nothing
  private." It is one constant (`SENT_WARNING` in `helia-feedback`). The failure
  message and tooltip say that Open in browser needs an account on
  github.com.
- The rail's Calibrate step is no longer "Planned #3"; every GUI screenshot in the
  book shows the new rail text.
- The FAQ and the troubleshooting page no longer say Helia has no code that makes
  network connections. That stopped being true with the feedback client: Helia
  connects only when you send feedback, and the stacker, the desktop app and
  `helia serve` otherwise make no connections of their own.

### Added

- `helia measure <image>` and `helia_core::measure::granulation_contrast`: the
  band-pass contrast of a disc-centre crop in a fine, a granulation and a meso
  band, for comparing settings on the same capture (#101). It is the measurement
  behind the 1.6 to 2.8 times figure above; see
  `docs/book/src/user-guide/quality-report.md`.

- The tone curve in the Adjust step can be dragged
  (#93, reported through
  the in-app feedback): click the plot to add a point, drag a point to move it,
  double-click it to remove it, and with the plot focused use `[` and `]` to choose
  a point, the arrow keys to nudge it (Shift for bigger steps) and Delete to remove
  it. A point stops at its neighbours (0.005 apart in input, between their
  outputs), so the curve always rises. See Editing the curve in
  `docs/book/src/user-guide/gui.md` and `docs/book/src/walkthrough/07-adjust.md`.
- Remembered settings, a log file and a bug report
  (#74): the app remembers
  the backend, export format, sigma clipping, full-size preview and the folders you
  last opened from and exported to. The app, `helia` and `helia serve` write a
  rotating `helia.log` in the OS log folder (file names and errors only, never
  image data or whole paths). **Save bug report** on the About page writes a `.zip`
  of version, GPU, capture header, settings and log tail to attach to an issue.
  `HELIA_LOG_DIR` moves the log. See `docs/book/src/user-guide/gui.md` (Settings
  file, Log file and bug report).
- Nightly workflows on the GPU runner
  (#62,
  #69): `real-data-nightly`
  (the real captures, hash-checked) and `nightly` (fuzzing with kept corpora, Miri,
  a benchmark regression check against a stored baseline, `cargo audit`), a new
  `video_probe` fuzz target, and a commit status per check on `main`.
  Miri runs the file readers and the edit parser (the workspace has no `unsafe`; the numeric
  tests took over an hour under Miri), in under 15 minutes.
- Robust stacking
  (#53): a Huber
  M-estimator (`--robust huber`) and a winsorised mean (`--robust winsor`)
  beside the existing sigma clip (`--robust sigma`, the same as `--sigma-clip`),
  with `--robust-k` for the threshold, `robust` and `robust_k` in an API job and
  Off, Sigma, Huber and Winsor chips in the app. Two streaming passes, memory
  flat in the number of frames; off by default, so the weighted mean is
  unchanged. The whole-frame stack runs on the GPU in all three robust modes
  (it was CPU only for sigma clipping). The Huber step down-weights a sample beyond
  `k` sigma of the weighted mean by `k` sigma over its distance (default `k` 1.345),
  the winsorised mean clamps it to `k` sigma (default 2) and the sigma clip drops it
  (default 2.5). Measured on 200 synthetic frames with a known answer (RMSE times
  1000): clean data 1.52 for the mean, 1.58 sigma, 1.55 Huber, 1.52 winsor; with 1%
  hot pixels 15.10, 1.49, 3.28 and 4.71; with 5% hot, 63.35, 3.28, 22.59 and 33.96.
  On a real stack the spread between frames is partly signal, so leave it off
  unless the capture has an artefact. See the user guide's How stacking works page
  (`docs/book/src/user-guide/how-stacking-works.md`, Stacking).
- Dark, flat and bad-pixel calibration
  (#3). Dust on the sensor
  window, etalon fringes and a camera's fixed pattern stay put while the Sun moves,
  so local alignment turns them into false detail and sharpening makes them worse.
  Frames are now calibrated as they are read, before they are demosaiced, scored or
  aligned, on the CPU and the GPU alike: `(raw - dark) / flat`, then hot, cold and
  stuck pixels replaced by the median of their same-colour neighbours.
  - `helia calibrate` builds a master dark, bias and flat (median or sigma-clipped
    mean; the flat is made from dark-subtracted frames, each scaled to its own
    level, and normalised within each colour of the mosaic) and a bad-pixel map from
    SER, AVI, video or FITS calibration captures, and writes them as float FITS
    files with their provenance in the header. It reads the flat for Newton rings,
    etalon fringes, vignetting (the H-alpha etalon's sweet spot) and dust, and
    warns. `--lights` also finds stuck pixels in a light capture, for cameras with
    no darks.
  - `helia stack --dark --flat --bad-pixels` (and `--flat-dark`, `--bias`,
    `--scale-dark`, `--no-cosmetic`) applies masters, or builds them on the spot
    from capture files or folders. A master from another camera, region or mosaic
    is refused before any work starts.
  - The Calibrate step of the app is real: pick masters or capture folders, see the
    frame before and after (split, draggable), read the warnings, save the masters.
    The rail shows what is in force. The picture and the quality scores follow the
    calibration.
  - The API takes `config.calibration` (`dark`, `flat`, `flat_dark`, `bias`,
    `bad_pixels`, `cosmetic`, `scale_dark`, `combine`), and a finished job reports
    what was applied in `summary.calibration`.
  - The masters are recorded in the output: `DARK-SUBTRACTION`, `FLAT-FIELDING` and
    `BAD-PIXEL-CORRECTION` `PRSTEP` cards and `HISTORY` lines in FITS, the
    description in TIFF and PNG. Each master carries a fingerprint of its pixels.
  - Known-answer tests on a synthetic sensor with injected vignetting, dust, a fixed
    pattern, hot and cold pixels: the stack's residual pattern falls by 36.9 dB with
    a dark and a flat (the bound the test enforces is 20 dB) and stays 34 dB down after
    Richardson-Lucy sharpening, which makes an uncalibrated pattern 6% worse. CPU and
    GPU parity tests cover the decode of calibrated frames and a whole calibrated
    stack.
  - User guide page, walkthrough step, API and command-line documentation, and
    screenshots of the step: `docs/book/src/user-guide/calibration.md` and
    `docs/book/src/walkthrough/calibrate.md`.

## [0.3.0]

Third pre-release. Helia can now take feedback from inside the app, and says plainly that it is a pre-release.

The feedback service at `https://feedback.discworld.casa` is new. If it is not answering, the feedback page says so and offers to save the report as a `.zip` or open the forge's new-issue page already filled in.

### Added

- Feedback from inside the app
  (#80): a **Feedback**
  button in the top bar (Ctrl+Shift+F, and a button on the About page) takes a
  screenshot of the window and opens a page where you draw rectangles round the
  problem, choose Bug, Feature request, Question or Other, write a title and
  description, optionally include system details (version, commit, build, GPU,
  operating system, the open capture's file name and header summary), preview
  exactly what will be sent, and send it. It is filed as a public issue through a
  new feedback relay at `https://feedback.discworld.casa`. If the relay cannot be
  reached you can save the report as a `.zip` (report, screenshot with and without
  the rectangles) or open the forge's new-issue page already filled in. Nothing is
  sent until you press Send, and there is no telemetry. See
  `docs/book/src/walkthrough/feedback.md`, `docs/book/src/user-guide/sending-feedback.md`
  and the protocol in `docs/book/src/reference/feedback-protocol.md`.
- `helia feedback --kind --title --description [--screenshot --highlight
  --system-info --dry-run --save --url]`, the same client for headless use.
- The `helia-feedback` crate: the relay protocol's types, the proof of work, the
  client and the fallbacks, and the protocol specified in the book, with a test
  vector. Authentication sits behind one interface so a client-certificate method
  can be added later.
- Environment variable `HELIA_FEEDBACK_URL` and the settings-file key
  `feedback_url` to point at another feedback service.

### Changed

- Helia identifies itself as a pre-release while its version is below 1.0.0: a
  Pre-release badge beside the name in the top bar and in the About page, the
  window title `Helia 0.x.y (pre-release)`, and a `release: pre-release` line in
  `--version` and the version block. One build-time rule decides it.

## [0.2.0]

Second pre-release. Helia now has guided steps and colour presets, sharpening
and adjustments, limb-fit alignment, SOLARNET FITS output, MP4 input, smarter
SER handling, a logo and an About page.

### Added

- Easy steps, notices and colour presets
  (#41):
  - `helia stack` does the right thing with no other flags: the filter is read
    from the file name, folder, SharpCap sidecar or SER header and sets the
    Bayer channel and the share of frames; the alignment point size comes from
    the image size. `settings:` says what was chosen and why. The filter is
    found by splitting the file name, folder, sidecar and header text into words
    (at punctuation, at a lower-case to upper-case change and between letters and
    digits) and matching whole words such as `ha`, `cak`, `589` in that order of
    trust, so a date like 2026-05-17 is not taken for a wavelength and `Hampshire`
    is not H-alpha. Nothing is hidden: every recommended value is an ordinary
    option that overrides it. See `docs/book/src/user-guide/quick-start.md`,
    `docs/book/src/walkthrough/02-quick-stack.md` and the Colourising page
    (`docs/book/src/user-guide/colourising.md`, How the filter is recognised).
  - Colour presets for H-alpha, Ca II K, Ca II H, Mg I b, Na I D, He I D3,
    H-beta and white light, with an optional invert and a strength mix
    (`--preset`, `--colour`, `--invert`, `--colour-strength`, `helia presets`).
    Colour is presentation: FITS stays mono and unaltered. The look is recorded
    in the file. Each look is a black-to-white ramp defined by a tone exponent and
    a gain and an exponent per colour channel, so a filter keeps a recognisable hue
    from shadows to highlights; the numbers are in the Colour presets reference
    (`docs/book/src/reference/presets-table.md`), and the tutorial is
    `docs/book/src/walkthrough/08-colour.md`.
  - PNG output (8-bit grey or RGB) in the command line, the app and the API.
  - Desktop app: a Quick stack card with one **Stack with recommended
    settings** button, a hint under every step, a dismissible first-run tips
    strip, and a Colour step with a swatch for each look, Invert, Strength and a
    live preview. The look last used for each filter is remembered.
  - Desktop app: a notice for every long operation with a progress bar or
    spinner, the time left and a Cancel button where stopping makes sense, and
    toasts for results (errors stay until dismissed). Both carry screen-reader
    text.
  - API: `GET /api/v1/presets`; `recommended` in the capture inspection;
    `preset`, `colour`, `invert` and `colour_strength` in a job's `config`;
    `summary.filter`, `summary.filter_source` and `summary.colour`; the stages
    `opening`, `colourise` and `write`.
- Sharpening and adjustments
  (#5): unsharp mask,
  wavelet layers with a noise cut, Richardson-Lucy deconvolution with a limb and
  sunspot ringing guard, wavelet denoise, levels, a tone curve, crop, rotate,
  flip and resize, as a recorded, non-destructive list (`helia_core::edit`) with
  recommended values for each filter. CPU reference plus GPU kernels with parity
  tests. Sharpening runs before colour; FITS stays the unsharpened stack unless
  asked (`--sharpen-fits`).
  - How: the wavelet method is an a-trous transform (the `1 4 6 4 1` / 16 spline,
    its taps spread twice as far at every step), with a gain and a noise cut per
    layer; the noise of a layer is measured from its median absolute deviation, so
    a few bright features do not move it, and gain 1 with cut 0 returns the input
    bit for bit. Deconvolution is Richardson-Lucy with total-variation
    regularisation (Dey et al., 2006) and a guard that, where a large step lies
    within reach of the blur (the limb, an umbra), holds the result inside the range
    of the original around that pixel.
  - Measured on synthetic images with a known truth: deconvolution lowers the RMS
    error against the truth from 0.0349 to 0.0163 for a Gaussian blur of sigma 1.8
    px and from 0.0180 to 0.0044 for an Airy blur; the guard cuts the overshoot at a
    synthetic limb from 16.7% to 1.1% of the step and at a sunspot from 10.9% to
    1.9%; denoise lowers flat-region noise from 0.0202 to 0.0048 while edge
    contrast goes from 0.401 to 0.390. On four real captures of 2022-07-17 the
    recommended wavelets raised Helia's detail metric by 1.22 to 1.25 times and took
    most of the finest layer's noise out; unsharp mask and deconvolution moved it by
    1.02 to 1.04 times. See `docs/book/src/user-guide/sharpening.md`,
    `docs/book/src/walkthrough/06-sharpen.md` and
    `docs/book/src/walkthrough/07-adjust.md`.
  - `helia stack --sharpen wavelet|unsharp|rl|none`, a flag for every value, the
    adjustment flags, saved presets (`--edits`, `--save-edits`), a before and
    after report, and `helia edit <image> -o <out>` for an existing TIFF, PNG or
    FITS.
  - Desktop app: working Sharpen and Adjust steps, a live preview that follows
    every slider, a draggable **Before / after** split and saved presets.
  - API: `sharpen`, `edits` and `sharpen_fits` in a job's `config`, an `edit`
    stage, and `sharpen` values in `GET /presets`.
- Limb-fit global alignment with a confidence
  (#18).
  `--align auto|phase|limb` (API `align`): when the whole disc is in the frame
  and the reference fit is confident, Helia fits a circle to the limb (robust,
  sub-pixel, with a jackknife error estimate); otherwise it uses phase
  correlation, which now reports a confidence too. The choice, the reason and
  the confidence appear in the CLI, the app and the API (`summary.alignment`). The
  limb is found along 720 rays sampled every half pixel, each edge taken as the
  centroid of the steepest stretch of its profile; the circle is fitted by
  iteratively reweighted least squares with a Tukey biweight, so prominences and
  sunspot edges are ignored, then refitted in a narrow window. The confidence comes
  from the larger of the scatter estimate and a jackknife of the centre. Measured on
  synthetic discs with known shifts: worst error 0.014 px over 12 random sub-pixel
  positions, mean 0.021 px and worst 0.066 px over 40 frames shifted by up to 15 px,
  and worst 0.020 px with 40% of the limb outside the frame. Against phase
  correlation on a real capture with a clean limb (`15_32_09.avi`) the two differ by
  a median 0.09 px, and in a global-only stack of four real captures the two gave
  the same sharpness within 0.5%. See `docs/book/src/walkthrough/04-align.md` and
  `docs/book/src/user-guide/how-stacking-works.md` (Aligning the whole frame).
- SOLARNET-compliant FITS output
  (#19): observation
  times from the SER timestamps, exposure, telescope, instrument and observer,
  filter and wavelength, the observing site (`--site LAT,LON,ALT`), a record of
  every processing step and an approximate helioprojective WCS from the limb
  fit. Keywords whose value is not known are left out, with a `COMMENT` saying
  so. A test builds a `sunpy.map.Map` from the output. Every keyword is
  documented under Output formats (`docs/book/src/reference/output-formats.md`;
  tutorial `docs/book/src/walkthrough/09-export.md`).
- MP4 and other compressed video through the optional `ffmpeg`
  (#40): MP4, MOV, M4V,
  MKV, WebM and compressed AVI open if `ffmpeg` and `ffprobe` (4.4 or newer) are
  installed. Helia stays pure Rust: they run as child processes and are never
  linked or bundled. The video is decoded once into a temporary SER cache
  (`--cache-dir`, `HELIA_CACHE_DIR`, with a free-space check). Lossy input
  carries a note that raw captures keep more detail. Typed errors for a missing
  or old ffmpeg, a truncated video and too little disk space. Video codecs do not
  seek by frame and the pipeline reads a capture several times in any order, which
  is why the decode happens once into an uncompressed SER cache that is then read
  like any SER file (GPU decode included) and removed when the last handle closes.
  The programs start from an argument vector, never through a shell, with the input
  as an absolute path and `-protocol_whitelist file`. See
  `docs/book/src/recipes/mp4-from-a-phone.md` and
  `docs/book/src/user-guide/capturing.md`.
- Header view, Bayer detection and overrides for SER captures
  (#47): `helia info`, the
  Open step and `POST /captures/inspect` show every SER header field as written,
  then the values Helia will use with their source (`header`, `sidecar`,
  `detection` or `override`). A Bayer mosaic in a SER file whose header says
  mono is detected, with a notice. Overrides: `--bit-depth`, `--endian`,
  `--flip-rows` and `--bayer auto|none|rggb|bggr|grbg|gbrg`, in the app and the
  API too. How the capture was read is recorded in every output file. Detection
  measures the mean of each of the four pixel positions of a 2 x 2 cell on the disc
  in a few frames spread through the capture: on a mono sensor they agree to well
  under a percent, behind a colour-filter array they do not. Helia reports a mosaic
  when they differ by 5% or more (between 2% and 5% it cannot tell and says so),
  names the pattern from the two green phases that agree with each other, and
  separates red from blue by the layout a SharpCap sidecar states, then the colour
  the filter passes, then the camera model, and finally brightness. See
  `docs/book/src/walkthrough/01-open.md`,
  `docs/book/src/recipes/fixing-a-bayer-grid.md` and
  `docs/book/src/user-guide/troubleshooting.md`.
- Authorship and About page (#39): Helia is copyright 2026 Laurence
  Horrocks-Barlow, with SPDX headers on every source file and
  `cargo xtask reuse-check` in CI. `helia --version` and `helia-gui --version`
  print the commit, build date, profile, target and toolchain versions, the
  copyright, licence and no-warranty notice; `GET /api/v1/version` reports the
  same. The app has an About page (the sun in the top bar, or F1) with a Copy
  button for bug reports. Windows executables carry a VersionInfo resource.
- Additional terms under GPLv3 section 7 (#45): `ADDITIONAL-TERMS.md` requires
  the author attribution and the dedication to be kept, and modified versions to
  be marked as changed. `LICENSE` is the unmodified GPL text. Release archives
  carry the file.
- The Helia logo and icons, all rights reserved (`LOGO-LICENCE.md`), in the
  README, the book, the window and About page, the Windows executable, the Linux
  archive (desktop file and hicolor icons) and the release notes
  (#63). The full logo is
  shown from the photographic PNG everywhere (README, book introduction and
  release notes), not the SVG. The code stays GPL.
- Release notes end with the copyright line and a pointer to the author's other
  work, and the SBOMs name the author and supplier.
- Documentation: verification of eleven candidate methods (issues #49 to #59),
  new pages for colourising, sharpening and adjustments, troubleshooting and
  capturing (`docs/book/src/developer/research.md`).

### Changed

- `--ap-size` and `--channel` (API `ap_size`, `channel`) no longer default to 64
  and `lum`: absent, they come from the recommendation. The `config` echoed by
  `GET /jobs` shows `null` for them.
- Desktop app: after opening a capture the Open step stays selected; after a
  stack the app goes to Sharpen, then Colour, then Export. Backend, demosaic and
  channel settings are behind **Advanced settings**. Export offers TIFF, PNG or
  FITS. Opening a different capture resets the overrides to Auto.
- `helia info` prints a longer layout: header as written, values used with their
  source, mosaic check and notices.
- The book and README state the copyright and licence; CONTRIBUTING explains how
  contributors keep their own copyright and add a header.

### Fixed

- A SER file whose header says mono but whose frames are raw Bayer data (a ZWO
  ASI482MC recorded by SharpCap) was stacked with its mosaic left in, so the
  export showed a grid
  (#47). It is now
  detected and debayered, with a notice.
- CI: the GUI snapshot tests run one at a time, and a flaky test-job failure on
  the runner is tracked in
  #37.
- CI: a GUI snapshot that differs is rendered once more before it fails, and the Windows
  job keeps the diff images as an artifact
  (#108).

### Fixed

- Flaky tests on the Linux and Windows CI lanes (#37).
  A failing `ci / test` or `windows / tests` job now posts the failing test names and the
  first panic line of each as a `ci / test-failures` or `windows / test-failures` commit
  status. The causes found and fixed:
  - `helia serve` died when the process reading its first output line closed the pipe
    (a broken-pipe panic on the second line), so a client could be refused or reset; the
    serve tests also closed that pipe themselves. Output errors are now ignored.
  - `GpuBackend::shared` tore the device down when the last user finished and opened a new
    one for the next, and the API tests did so on every `/version`: software Vulkan
    (lavapipe) hung with its threads spinning, and test binaries crashed. A software
    device is now opened once per adapter request and kept for the life of the process;
    a hardware device is still released when its last user finishes, because keeping an
    NVIDIA device alive to exit made the process die with a signal on the way out.
  - The GUI snapshot tests each created and destroyed a wgpu instance (including the GL
    backend, which panics with `BadAccess` across threads); they share one.
  - The WebSocket progress test assumed it would see the job change to `running`, which it
    cannot when the job started before the socket connected.

## [0.1.0]

First release: a pre-release of the stacking pipeline, command line tool, API
server and desktop app. Linux x86_64 and aarch64 and Windows x86_64 archives are
built by the Forgejo runners.

### Added

- Compiled releases (#35). Pushing a protected `v*` tag runs
  `.forgejo/workflows/release.yml`, which builds `helia` and `helia-gui` for
  Linux x86_64 and aarch64 (glibc 2.35 baseline), checks each binary runs and
  reports the tag's version, and publishes a Forgejo release with
  `helia-<version>-<target>.tar.gz` archives, `SHA256SUMS`, CycloneDX SBOMs and
  release notes taken from this file. Each archive carries
  `THIRD-PARTY-LICENSES.txt` (cargo-about). `release-windows.yml` builds the
  Windows zip on the self-hosted Windows runner and attaches it to the release, and
  can add it to a release that is already out. See the Releasing page of the book.
- `helia-gui --version` (and `-V`) prints the version and exits without opening
  a window.
- `helia-core`: SER v3 reader and writer (8 and 16 bit, mono, Bayer and RGB,
  timestamps), with a byte-order check for writers that mislabel the
  endianness flag (SharpCap files have been seen with the flag saying big-endian
  while the data is little-endian; when the flag says big-endian the reader looks
  at the first frame and decodes it the way that gives a smooth image). See `docs/book/src/user-guide/capturing.md` and the SharpCap section of
  `docs/book/src/user-guide/troubleshooting.md`.
- `helia-core`: streaming reader for uncompressed AVI (`DIB`/`BI_RGB`, `Y800`,
  `GREY`, `Y16`, 24-bit BGR), covering AVI 1.0 and OpenDML `AVIX` files, bottom-up
  and top-down rows and 4-byte row padding. SharpCap `CameraSettings.txt`
  sidecars supply the Bayer pattern hint.
- `helia-core`: minimal FITS reader and writer, and a 16-bit TIFF writer.
- `helia-core`: bilinear and Malvar-He-Cutler demosaicing with red, green, blue
  or mean-channel output. Malvar-He-Cutler (2004) is a 5 x 5 linear filter that
  corrects the bilinear estimate of each missing colour with a gradient measured in
  another colour at the same pixel, which keeps edges sharper and is the default;
  a test requires its RMS error on a sharp synthetic edge to be below bilinear's.
  H-alpha users usually want the red channel only, because behind that filter the
  blue and green pixels add noise, not detail. Later versions fold the filter
  stencil into the byte conversion (see Performance below). See
  `docs/book/src/recipes/fixing-a-bayer-grid.md` and
  `docs/book/src/user-guide/colourising.md`.
- `helia-core`: gradient-energy and Laplacian-variance sharpness metrics with a
  noise floor, ranking, `--keep` selection and soft quality weights. Both metrics
  run on a frame blurred with a 3 x 3 binomial kernel (`[1 2 1] x [1 2 1] / 16`)
  and are divided by the squared mean of the blurred region after subtracting a
  per-frame noise floor, so noise does not raise the score and neither a brightness
  change nor a sky pedestal reads as sharper. Gradient energy, the mean squared
  difference of neighbouring pixels, is the default. The kept frames carry a weight
  from 0.2 (the worst kept) to 1 (the best) on a smooth S-curve, so borderline
  frames still help with the noise. See `docs/book/src/walkthrough/03-analyse.md`
  and `docs/book/src/user-guide/how-stacking-works.md` (Ranking the frames).
- `helia-core`: FFT phase-correlation global alignment with windowing,
  high-pass, sub-pixel refinement and a peak-to-sidelobe confidence. The frame is
  mean-subtracted, Hann-windowed and zero-padded to a 5-smooth size; the whitened
  cross-power spectrum is band-passed (a mild Gaussian high-pass against
  illumination gradients, a low-pass against noise), inverse-transformed for the
  integer peak, and the position refined by fitting a parabola to the correlation
  surface on a 3 x 3 grid, repeated so the parabola's bias vanishes. On a textured
  synthetic capture of 40 frames the worst shift error is 0.048 px. See
  `docs/book/src/walkthrough/04-align.md` and
  `docs/book/src/user-guide/how-stacking-works.md` (Aligning the whole frame).
- `helia-core`: multi-point alignment with per-point frame selection,
  ridge-aware local shifts, neighbour interpolation for unreliable points and
  cosine-feathered blending. A grid of overlapping square windows (64 px, half
  overlap by default) covers the reference; for each frame and each window with
  enough contrast, zero-normalised cross-correlation within a small radius around
  the global shift, refined to sub-pixel with a parabola, gives the local shift, and
  the window's own sharpness in that frame ranks it. Each window stacks only its
  best frames and the per-window stacks are merged with raised-cosine weights that
  sum to one. A point that is missing, below the correlation threshold or on a
  straight stretch of limb (the shift is fixed across the edge but not along it)
  takes the inverse-distance average of its neighbours' shifts instead of guessing.
  Measurement works on the raw frame without resampling, so the shift has no
  interpolation bias, and correlates a smoothed copy with the slow gradient
  removed. The measured benefit on real captures is in the 0.5.0 entry for
  granulation statistics (on all four sodium captures the granulation contrast of a
  selected 12% with alignment points ranks above global alignment). See
  `docs/book/src/walkthrough/04-align.md` and
  `docs/book/src/user-guide/how-stacking-works.md` (Aligning small patches).
- `helia-core`: weighted-mean stacking, optional sigma clipping and a splat
  accumulator interface ready for drizzle (currently `Unsupported`). Each point's
  chosen frames are moved by their measured shifts, sampled at sub-pixel positions,
  and combined with the quality weights. Sigma clipping works in two passes over the
  frames (mean and spread per pixel, then the sum without the samples beyond `k`
  sigma), so memory stays proportional to the pixel count whatever the number of
  frames; a pixel with fewer than three samples is left alone. Later versions add
  Huber and winsorised variants (0.4.0). See
  `docs/book/src/walkthrough/05-stack.md` and
  `docs/book/src/user-guide/how-stacking-works.md` (Stacking).
- `helia-core`: end-to-end `Pipeline` with progress callback and cancellation,
  rayon parallelism and bounded memory.
- `helia-core`: synthetic solar captures with known shifts, warps, blur and
  noise for known-answer tests; criterion benchmarks for phase correlation and
  stacking.
- `helia-gpu`: wgpu backend with WGSL shift-and-add accumulation and sharpness
  kernels, adapter selection with a software fallback, and CPU-versus-GPU
  parity tests controlled by `HELIA_GPU_TESTS`.
- `helia-cli`: `helia stack` and `helia info` with progress bar.
- `helia-server`: HTTP/JSON API under `/api/v1` (axum, tokio) for capture
  inspection, frame previews, queued stacking jobs with progress, cancellation,
  result download and a WebSocket event stream, plus a generated OpenAPI
  document (`docs/book/src/user-guide/api.md`, `docs/book/src/reference/api.md`). Loopback by default, bearer token (constant-time compare), every
  path confined to a data root (traversal, absolute and symlink escapes are
  `403`), request and queue limits, no CORS unless asked. See the API chapter of the book.
- `helia-cli`: `helia serve` runs the API server (`--bind`, `--data-root`,
  `--token`/`--token-file`, `--workers`, `--max-queue`, `--cors-origin`).
- `helia-gui`: egui application to open a capture, scrub frames, view the
  quality graph and alignment point grid, stack in the background with cancel,
  and save TIFF or FITS. See `docs/book/src/user-guide/gui.md` and the Step by step
  walkthrough (`docs/book/src/walkthrough/index.md`).
- `helia-gui`: embeds IBM Plex Sans and Mono (SIL OFL 1.1) in place of egui's
  bundled Ubuntu font, whose licence is not GPL-compatible.
- `helia-gui`: redesigned window with a dark amber theme (tokens in one module),
  a seven step pipeline rail with live status, a viewer with zoom, pan and
  alignment point overlay, a frame scrubber, a sorted quality curve with the
  keep threshold, and a per step inspector (metric, point size, keep share,
  sigma clipping, export format). The GPU adapter name is shown in the top bar.
  Side panels collapse in narrow windows; keyboard shortcuts for scrubbing,
  switching frame and stack, open and export; AccessKit names on every control.
  Decoding, scoring, stacking and file dialogs all run off the UI thread.
- `helia-gui`: window snapshot tests (egui_kittest, software GPU, gated by
  `HELIA_GPU_TESTS=software`) whose images feed the user guide.
- Documentation: the Helia book (mdBook, `docs/book`). User guide (installation,
  quick start, capturing, desktop app, command line, API, how stacking works,
  GPU acceleration, troubleshooting), reference (CLI, API, output formats,
  configuration), developer guide (architecture, building and testing, GPU
  parity, contributing, releasing, research) and the roadmap.
- `xtask`: `cargo xtask docs` generates the CLI reference (from the clap
  definition, now exposed by a `helia_cli` library target), the API reference
  and `openapi.json` (from the server's OpenAPI document); `cargo xtask docs
  --check` fails when the committed copies are stale and runs in both CI
  workflows. `cargo xtask sample` writes a synthetic SER capture to try Helia
  without a telescope.
- CI builds the book with `mdbook-linkcheck` (offline link, anchor and image
  checking); `.github/workflows/docs.yml` deploys it to GitHub Pages once the
  mirror exists.
- Pull request templates (`.github` and `.forgejo`) and a documentation rule in
  `CONTRIBUTING.md`: every change re-checks all docs, updates pages and
  screenshots, updates the changelog and runs `cargo xtask docs`.
- `helia-server`: operation summaries, error response descriptions and correct
  query parameter locations in the OpenAPI document.
- Desktop app walkthrough with screenshots produced by the snapshot tests.
- Fuzz targets `ser_reader`, `fits_reader` and `avi_reader`.
- Real-capture integration tests gated by `HELIA_REAL_DATA`.
- `docs/ARCHITECTURE.md`, included in the book.
- Alignment stays off burned-in text and empty sky (#16). `helia-core::mask`
  finds burned-in text (pixels that flip between white and dark in a small area
  at a frame edge), fits the limb with a robust circle, and wraps the capture so
  masked rectangles read as one flat level on the CPU and the GPU. Alignment
  points are used only when their centre is inside the limb and their window
  overlaps no masked rectangle. New `helia stack` options `--exclude x,y,w,h`
  (repeatable), `--prominence-band <px>`, `--no-disc-mask` and
  `--no-overlay-detect`; the same four settings in the API job config
  (`exclude`, `prominence_band`, `disc_mask`, `detect_overlay`); an *Exclude
  regions* list in the app's Align step. Masked areas are flat in the result. The
  limb is a robust circle fit; text is found as pixels that flip between white and
  dark in a small area at a frame edge (the strip rule was tightened in #118, see
  Unreleased). See `docs/book/src/user-guide/how-stacking-works.md` (Keeping
  alignment off text and sky) and `docs/book/src/user-guide/troubleshooting.md`.

### Performance

- `helia stack --timings` prints wall time per stage (open, scoring, reference,
  global alignment, alignment points, stacking, finish, write) and busy time
  per kind of work (decode, debayer, quality, global align, AP align, stack,
  finish) summed over threads. The same figures are in
  `StackOutput::busy` (`helia_core::profile`).
- Frames are decoded concurrently straight from the capture file with
  positional reads (`io::raw`), so every thread reads, converts and demosaics
  its own frame; sources without such a decoder keep the sequential path.
- Demosaicing is one fixed 5x5 stencil per Bayer phase, applied while the
  stored bytes are converted (`Debayer::apply_rows`), instead of three
  padded interpolation passes. Output matches the old code to 2e-6.
- Sharpness scoring is row-based and allocation-light, and window sharpness
  for alignment points no longer re-blurs each window (`WindowScorer`).
- Alignment point correlation uses running sums for the normalisation and a
  vectorised dot product for the numerator; `measure` and `fill_missing` take
  the new `Prepared` view of a frame.
- Resampling filters each source row horizontally once per region instead of
  once per output row (bit-identical results, about 9x faster per tile).
- Global phase correlation transforms only the non-negative half of the
  spectrum of the real frame (`realfft`), keeps its plans, filters and
  buffers for reuse across threads, and takes its first refinement step from
  the correlation surface; shifts agree with the previous implementation to
  1e-6 pixel. `build_reference` registers its frames in parallel.
- The pipeline hands whole stages to the backend (`Backend::score_frames`,
  `register_frames`, `measure_points`, `stack_frames`); a backend that declines
  one gets the CPU implementation. Registration and alignment point
  measurement share one pass over the candidate frames on the CPU.
- `helia-gpu`: frame scoring runs on the device from the stored bytes: a WGSL
  kernel converts and demosaics (the same 5x5 stencils as the CPU), then blur,
  reduce and noise-floor sampling follow, in batches while a helper thread
  reads the next batch from the file. Parity tests cover every stored layout.
- `helia-gpu`: alignment point measurement runs on the device: separable
  Gaussians (smoothing and background removal), a 3x3 blur and gradient-energy
  table for window scoring, and a zero-normalised correlation of every active
  point's template over its search window, for a batch of frames at a time.
  The peak fit and the neighbour interpolation of unreliable points run on the
  host with the CPU's own code (`fit_peak`, `plan_fill`), so decisions agree.
- `helia-gpu`: mean stacking runs on the device: the whole-frame sums and
  every alignment point tile accumulate from decoded batches without leaving
  the device until the end (tap weights computed on the host, as before).
- `helia-gpu`: global registration runs on the device: an in-place mixed-radix
  (2, 3, 4, 5) FFT in workgroup memory along rows and columns of the half
  spectrum, the cross-power whitening, the inverse transforms, the integer peak
  and the sub-pixel refinement, all on the sizes and filter tables of the CPU
  correlator. Shifts agree with it to about 1e-5 px. Sizes beyond 2048 points
  stay on the CPU.
- `helia-gpu`: the correlation kernel keeps three offsets per thread and reads
  a frame padded by two columns, about 1.5 times faster than the first
  version; kernels no longer zero-fill workgroup memory, which took the WARP
  adapter 40 s to compile each FFT kernel. `HELIA_GPU_TRACE=1` also prints
  the GPU time of every kernel (timestamp queries) at the end of each stage.
- `helia-gpu`: adapter search tries the native API (Vulkan, Metal) before
  starting every backend, which cut device start-up from about 1.5 s to 0.4 s;
  `--backend auto` no longer picks a software adapter. `HELIA_GPU_TRACE=1`
  prints where the device stages spend wall time.
- `helia-gpu`: scoring and alignment point measurement keep two batches in
  flight, so the device works on one while the host finishes the previous one
  (file reads already overlapped); window sums read columns so neighbouring
  threads read neighbouring samples (15 times faster).
- Criterion benchmarks for the new hot paths: debayer, alignment point
  preparation and correlation, window scoring, resampling, sharpness and phase
  correlation at full HD in `helia-core`, and the four device stages in
  `helia-gpu` (`cargo bench -p helia-gpu`, which skips itself without an
  adapter; `HELIA_GPU_BENCH=software` uses the software one).
- `cargo run --release --example tiffdiff -- a.tif b.tif` (in `helia-cli`)
  compares two stacked images by RMS difference and detail score.

### Not yet implemented

- Drizzle (1.5x and 2x): the interface exists and returns an `Unsupported` error.
- Dark and flat calibration, two-pass alignment point refinement, sharpening.

### Fixed

- The GPU parity test binary no longer crashes with SIGSEGV at exit on the
  NVIDIA A100 (#31). The shared device is now torn down in order when the last
  user drops it, after waiting for its queue, instead of living in a static that
  the driver unloaded underneath. The wgpu instance is kept for the life of the
  process: destroying it unloads the driver libraries while threads that called
  into them still have thread-local destructors pointing there, and such a
  thread crashes when it exits.
- The `helia-server` API tests no longer hang with one thread at 100% CPU when
  they run in parallel (#31). Concurrent Vulkan device creation and destruction
  (each `/version` probe built and dropped a whole device) is serialised, and
  every caller in a process shares one device. The nightly GPU job runs without
  `--test-threads=1` and without its SIGSEGV tolerance.
- The frame readout in the app (`quality 0.90 · rank 694`) disagreed with the
  rank because quality was a ratio to the best score, which barely moves when the
  scores are close together. It is now the position between the worst frame (0)
  and the best (1), the scale of the sorted curve, so it falls steadily with
  rank (#16).
- The GPU chip's tooltip shows the full adapter name, and a long name is cut
  without a dangling space before the ellipsis (#16).
- `queue_is_fifo_bounded_and_queued_jobs_cancel` waits for its first job to run
  before it checks queue positions.

### Changed

- `helia-cli`: the `--output` help mentions `.fts`, which was already accepted.
- `docs/api.md` moved into the book (`docs/book/src/user-guide/api.md`).
- `--backend auto` never picks a software adapter (lavapipe, WARP, llvmpipe); with
  no hardware GPU it uses the CPU path (#28). Tests can still ask for one with
  `HELIA_GPU_TESTS=software`.
- `GpuBackend::shared` gives every caller in a process the same device; the
  CLI, the API server and the app use it (`select_backend` and the `/version`
  probe included). `GpuContext` waits for its queue and releases the device and
  adapter when dropped; the instance stays.
- Alignment points that were placed outside the disc are no longer used by
  default; use `--prominence-band` for prominences or `--no-disc-mask` for the
  old behaviour.
