# Solar lucky imaging: models, methods and recommendations for Helia

Status: research review. Audience: Helia designers (open-source, Rust, wgpu solar stacker).

## 0. How to read this document

Every non-obvious claim carries a citation. Two tags show how far each claim was checked:

- **[V]** the source page, abstract or code was read (or its relevant text appeared in a search result) during this review.
- **[M]** stated from background knowledge of the literature and not re-checked here. Treat as "probably right, confirm before relying on it". Bibliographic details marked [M] (volume, page) are the most likely to contain slips.

Where I looked and found nothing, the text says so. Notably, **I found no peer-reviewed head-to-head comparison of frame-quality metrics on solar data** beyond Denker et al. (2018), and **no documented amateur use of speckle masking or MFBD on solar video**. Both gaps are stated again where they matter.

The section order follows the brief (1 to 10), then the recommended pipeline and test plan.

---

## 1. Atmospheric model

### 1.1 Kolmogorov turbulence and the Fried parameter

Refractive-index fluctuations in the turbulent atmosphere follow Kolmogorov statistics (inertial-range structure function proportional to r^(2/3)), which gives a phase power spectrum proportional to |k|^(-11/3) [M]. Fried (1966, JOSA 56, 1372, "Optical resolution through a randomly inhomogeneous medium for very long and very short exposures") [M] defined the coherence diameter r0: the aperture over which the rms phase error is about one radian. Two consequences matter for design:

- r0 scales as lambda^(6/5), so seeing is better in the red than the blue. A lecture-level statement of the scaling and of a typical night-time r0 of about 10 cm at 500 nm is in [Sheffield PHY217 L10](https://slittlefair.staff.shef.ac.uk/teaching/phy217/lectures/telescopes/l10) [V, secondary]. Daytime solar r0 is generally smaller and varies from a few centimetres in poor conditions to roughly 10 cm or more at good sites; I could not retrieve a primary table of daytime values (Rimmele & Marino 2011 is the right reference, see below) so numbers here are order-of-magnitude [M].
- Seeing-limited FWHM is about 0.98 lambda/r0. For r0 = 5 cm at 550 nm that is about 2.2 arcsec, whereas a 100 mm aperture has a diffraction FWHM near 1.4 arcsec at 550 nm (about 1.65 arcsec at H-alpha, using the 1.22 lambda/D Rayleigh radius). Hence a small amateur aperture is often within a factor of two of its diffraction limit even in moderate seeing, which is why simple lucky imaging works well for amateur solar apertures and why large apertures (D much greater than r0) benefit most from speckle or MFBD.

The key reference for the physical regime of daytime solar turbulence and its correction is Rimmele & Marino (2011), "Solar Adaptive Optics", [Living Reviews in Solar Physics 8, 2](https://doi.org/10.12942/lrsp-2011-2) [V for existence and scope, numbers not retrieved].

### 1.2 Tip-tilt dominates, high orders are what lucky imaging selects

Noll (1976, JOSA 66, 207, "Zernike polynomials and atmospheric turbulence") [M] gives the Kolmogorov Zernike variances: about 87 per cent of the uncorrected phase variance is in global tip and tilt. This explains the two-part structure of every lucky-imaging pipeline: first remove image motion (registration), then select or weight frames by how little high-order aberration they contain. It also explains why a pure shift-and-add (registration without selection) already helps: motion blur is the largest single term.

### 1.3 Probability of a good frame

Fried (1978), "Probability of getting a lucky short-exposure image through turbulence", [JOSA 68, 1651](https://doi.org/10.1364/JOSA.68.001651) [V for citation]. A secondary source quotes the approximation P ≈ 5.6 exp(-0.1557 (D/r0)^2) for the chance of a near-diffraction-limited short exposure ([arXiv:1603.00729](https://arxiv.org/pdf/1603.00729), secondary, check constants against Fried before use). The exponential in (D/r0)^2 is the design-relevant fact: the useful fraction collapses fast as D/r0 grows. For a 100 mm refractor with r0 of 5 cm (D/r0 = 2) the exponent is small and most frames are usable; for a 300 mm aperture (D/r0 = 6) good frames are rare and local, not global, selection becomes essential.

Law, Mackay & Baldwin (2006), ["Lucky imaging: high angular resolution imaging in the visible from the ground", A&A 446, 739](https://arxiv.org/abs/astro-ph/0507299) [V], demonstrated the method experimentally on a 2.56 m telescope: near-diffraction-limited images in good seeing and a resolution gain of roughly 2.5 to 4 in poorer seeing, with selection fractions chosen by the guide-star measure. That work is nocturnal and uses a bright-star Strehl proxy; the Sun supplies no such star, which is the central difficulty for Section 3.

### 1.4 Isoplanatic patch: why the Sun forces local processing

The isoplanatic angle is the field over which the PSF can be treated as constant. For turbulence concentrated near the ground (the daytime case, driven by solar heating) the patch is small. Evidence:

- A 1990s Dutch Open Telescope (DOT) speckle proposal states a daytime isoplanatic patch of about 5 arcsec ([Rutten, DOT speckle proposal 1992](https://robrutten.nl/rrweb/rjr-pubs/1992-dot-speckle-proposal.pdf)) [V, via search snippet].
- A Big Bear Solar Observatory proposal puts the patch at roughly 10 arcsec in visible light in good seeing, and notes that flares and CMEs span fields about ten times larger ([BBSO, ATI 2013](https://bbso.njit.edu/pub/staff/pgoode/ATI-2013/ati13_c.pdf)) [V, snippet]; Rimmele's DST adaptive-optics paper calls the patch "a severe limitation" for extended solar fields ([ao4elt proceedings](https://ao4elt.edpsciences.org/10.1051/ao4elt/201008002/pdf)) [V, snippet].
- MFBD practice uses sub-fields of about 4 to 5 arcsec square because space-invariance of the PSF holds only inside an isoplanatic patch ([arXiv:2205.13650](https://arxiv.org/pdf/2205.13650v1)) [V, snippet].
- A 2017 A&A study states that reaching the diffraction limit both inside and outside the isoplanatic patch requires post-facto reconstruction ([A&A 2017, 10.1051/0004-6361/201731275](https://www.aanda.org/10.1051/0004-6361/201731275)) [V, snippet; title not retrieved].

Arithmetic for the full disc (mine): the disc subtends about 1920 arcsec across, area about 2.9 million square arcsec. A 5 arcsec patch tiles it about 116,000 times, a 10 arcsec patch about 29,000 times. The PSF (and the image displacement) is therefore different over tens of thousands of independent regions in every frame, and a global registration shift is only the mean of a smoothly varying displacement field. This is the physical justification for multi-point (alignment-point, AP) processing: AP size should be tied to the isoplanatic scale, not chosen as a pixel count. In plate-scale terms, an AP box of about 5 to 10 arcsec is 17 to 33 px at 0.3 arcsec/px.

Practical corollaries:

1. The largest anisoplanatic term is differential image motion (local tilt). That is exactly what a local-shift AP grid removes (Section 4).
2. Higher-order anisoplanatic blur varies on the same scale but cannot be removed by shifting; only per-patch frame selection (local lucky imaging) and per-patch restoration (speckle, MFBD) address it.
3. A solar disc has structure of strongly varying contrast (limb, granulation, pores, plage, filaments); APs on flat or dark regions yield unreliable shifts and must be rejected or filled from a smooth interpolation (PSS and Siril both do this, Section 4).

### 1.5 Time scales and frame rate

Denker et al. (2018), ["Image Quality in High-resolution and High-cadence Solar Imaging", Solar Physics](https://arxiv.org/abs/1802.00760) [V abstract], analysed 160 Hz G-band and blue-continuum HiFI sequences at the 1.5 m GREGOR telescope and concluded that fast cadence and millisecond exposures are not enough to match the coherence time of daytime seeing, and that acquisition rates above about 50 Hz are needed to capture a substantial fraction of the best-seeing moments. Their standard HiFI mode selects 2 x 100 frames out of a 10 second burst. Short-exposure frame selection for solar work goes back to Scharmer (1989) and Scharmer & Löfdahl (1991), cited in that paper [V, secondary].

Design consequences: (a) exposure should be at or below the seeing coherence time (a few milliseconds; I did not retrieve a primary value so this is [M]); (b) the 50 Hz floor is a pertinent target for the capture guidance Helia gives users; (c) because the frozen-turbulence lifetime is short, consecutive frames are strongly correlated only at the highest rates, which affects how many *independent* samples a capture contains.

---

## 2. Capture

Helia is mostly a processing tool, but its design must state what input it expects, because capture decisions set the information ceiling.

### 2.1 Exposure, gain, bit depth, ROI, frame rate

- **Exposure**: short enough to freeze the seeing (Section 1.5); in white light this is easy (sub-millisecond to a few ms with a safe-filtered refractor), in H-alpha through a narrowband etalon it is photon-limited and the trade-off between exposure and noise becomes the main constraint [M].
- **Gain**: raise gain until the histogram sits at roughly 50 to 70 per cent of full scale but never clip. Lucky imaging favours more frames at higher gain over longer exposures, because stacking averages read noise down and selection removes motion-blurred frames. Quantitative gain advice is camera-specific [M].
- **Bit depth**: 12-bit sensors stored as 16-bit, or 8-bit from USB-limited modes. Granulation contrast is only a few per cent in white light at the native resolution, so 8-bit quantisation (about 0.4 per cent per step) is a real limit on a single frame, though stacking dithers it. Helia should read 8 and 16-bit natively and accumulate in f32.
- **ROI and frame rate**: frame rate scales almost inversely with ROI area on USB cameras. Capturing a region of interest at 100 to 300 fps is the standard route to the 50 Hz+ regime of Section 1.5. A full-disc capture at 32 arcmin needs very short focal length and gives coarse sampling; high-resolution work is therefore ROI-based and full-disc work is mosaic-based (Section 8).
- **Duration**: because the Sun evolves, granulation changes on a time scale of minutes; derotation matters for long captures or mosaics (Section 8.5).

### 2.2 Sampling: f-ratio versus pixel size

An imaging system with circular aperture D has an optical cutoff frequency D/lambda (cycles per radian). Nyquist sampling of that cutoff needs a pixel pitch p <= lambda F / 2 where F is the focal ratio, i.e. F >= 2p/lambda (standard sampling theory, e.g. Goodman, *Introduction to Fourier Optics*) [M]. Worked values:

| lambda | pixel p | Nyquist F-number |
|---|---|---|
| 550 nm (white light) | 3.76 um | 13.7 |
| 550 nm | 2.4 um | 8.7 |
| 656.3 nm (H-alpha) | 3.76 um | 11.5 |
| 393.4 nm (Ca II K) | 3.76 um | 19.1 |

Seeing-limited data are blurred below the diffraction limit, so for poor seeing slight undersampling loses little; for good seeing and best results slightly oversampling (F up to about 1.5x Nyquist) lets sub-pixel registration and sharpening work well. Amateur community rules of thumb (F ratio about 3 to 5 times pixel size in microns for planets) are consistent with this; I did not find a primary solar-specific source. Helia should (a) compute and display the sampling ratio q = lambda F / (2 p) from user-entered optics, (b) warn when q < 1 (undersampled, drizzle becomes relevant, Section 5.3), and (c) set AP sizes and wavelet scales in arcseconds or in units of the diffraction FWHM, not in pixels.

### 2.3 Calibration frames: darks, flats, and the H-alpha etalon

- **Darks / bias**: matter little for bright white-light frames at short exposure, but a master dark removes hot pixels and fixed-pattern noise that otherwise stack coherently when the image shifts less than the noise. Needed for H-alpha, where signal is low.
- **Flats**: essential for solar work because dust shadows on the sensor window and filter are conspicuous on a smooth disc and, after local de-warping, can be misread as granulation. Because the Sun shifts relative to the sensor during a capture, dust stays fixed while the image moves, so a good flat removes it, and a poor flat leaves a fixed pattern that the multi-frame process *sharpens*.
- **Newton's rings and etalon fringes**: in etalon-based H-alpha systems the interference between flat surfaces (etalon, camera window) produces fringes ([dehilster.info on removing Newton rings](https://dehilster.info/astronomy/removing_newton-rings.php) [V, snippet]). A British Astronomical Association overview of narrowband solar equipment discusses the same ([BAA](https://britastro.org/?p=215311)) [V, listing only]. One amateur observation notes that flats made by defocusing do not reproduce the ring pattern because the rings shift with focus; flats should be taken at the *same* focus and tilt as the science data, e.g. by moving the telescope across the disc (a "flat from the sun's own image" using a large random displacement, then median-combined) [M].
- **Etalon sweet spot**: a Fabry-Perot etalon's pass-band shifts with the angle of incidence of the light, so uniform filtering across the field requires an almost telecentric beam. One source: "The etalon will only give reasonably consistent filtration across an image if the light is almost parallel" ([scopetrader/H-alpha build discussion](https://scopetrader.com/building-a-hydrogen-alpha-solar-telescope/)) [V, snippet]. Practical effect for software: the disc has a brightness gradient and bandpass-driven contrast changes across the field (the "sweet spot"), so Helia needs (a) a flat/gradient normalisation prior to quality ranking, and (b) an option to compute quality only inside a user mask.

### 2.4 File formats: SER versus AVI versus FITS

| Format | Pros | Cons | Helia stance |
|---|---|---|---|
| SER | Designed for astro video; raw 8 to 16-bit; Bayer codes; per-frame UTC timestamps in an optional trailer; no compression ([Hahn, SER format](https://grischa-hahn.homepage.t-online.de/astro/ser/index.htm) [V]; 178-byte header per [Siril docs](https://siril-localized-doc.readthedocs.io/fr/stable/_sources/file-formats/SER.rst.txt) [V, snippet]) | No formal version tag: ColorID values 0 to 19 are version 2, 100 to 101 version 3 ([FFmpeg ticket](https://trac.ffmpeg.org/ticket/6821) [V, snippet]); endianness flag is historically inconsistent across capture programs [M] | MVP primary input |
| AVI | Ubiquitous | Compressed or demosaiced in many cameras, lossy codecs destroy fine detail and add blocking that shifts quality metrics, hard to get frame timestamps | Later, via an optional ffmpeg path only; warn user about lossy codecs |
| FITS | Scientific standard, header metadata, 16/32-bit float, cubes | Rarely produced by capture software as video; large | MVP for stacked outputs and cubes; frame-cube input as secondary |
| TIFF / PNG sequences | Simple | Many files, timestamps lost | Optional |

The SER trailer timestamps are valuable for solar work: they allow derotation and seeing-versus-time plots. Helia should read the header defensively (check file length against `frame_count * frame_size`), honour the endianness flag but also sanity-check it against pixel histograms, and fall back to file modification time if the trailer is absent.

---

## 3. Frame and region quality metrics

### 3.1 Candidate metrics

For a frame or an AP patch I:

| Metric | Formula (sketch) | Notes |
|---|---|---|
| Gradient energy / Tenengrad | sum of (Gx^2 + Gy^2) with Sobel, optionally thresholded | Responds to edges; noise adds a positive bias, so noise floor subtraction is needed. |
| Laplacian variance | var(Laplace(I)) | Used by PSS ("local_contrast_laplace") and Siril; strongly noise-sensitive because the Laplacian amplifies high frequencies. |
| Normalised variance | var(I)/mean(I) | Cheap; sensitive to illumination and to low-frequency structure, less to seeing. |
| Fourier band power | integral of |I(k)|^2 for k in [k1,k2] | Directly targets the spatial scales that seeing destroys; needs a chosen band (e.g. between 0.2 and 0.8 of the diffraction cutoff) and a noise-floor estimate; GPU-friendly with FFT. |
| RMS contrast of granulation | std(I)/mean(I) on a flattened region | The scientifically meaningful quantity in white light; compared to simulation in speckle work. Contrast of granulation saturates at the true (instrument-smoothed) value, so it tracks seeing only monotonically. |
| Median-filter gradient similarity (MFGS) | similarity between gradient images of the frame and its median-filtered version | Used for GREGOR HiFI quality assessment ([Denker et al. 2018](https://arxiv.org/abs/1802.00760) [V]); original definition and authorship not verified here. |

What the implementations actually do:

- **AutoStakkert!3** offers gradient-based and Laplace-based estimators with a "noise robust" parameter; default 4, raise it for noisy data, lower it for excellent data to concentrate on small features; a local (AP) estimator ranks frames at each AP and keeps (by default) the best 50 per cent per AP ([community guides summarised via search](https://www.iceinspace.com.au/forum/showthread.php?p=1393029), [autostakkert.com](https://www.autostakkert.com/wp/?p=60)) [V, snippets; no official manual retrieved].
- **PlanetarySystemStacker** provides three: "xy gradient" (mean finite-difference gradient magnitude after striding), "Laplace" (standard deviation of the Laplacian of the downsampled frame; the docstring says variance but the code returns the standard deviation) and "Sobel"; qualities can be divided by the frame's average brightness ([`alignment_points.py`, `miscellaneous.py`](https://github.com/Rolf-Hempel/PlanetarySystemStacker)) [V, code read through a fetch].
- **Siril** uses a Laplacian measure with brightness normalisation on by default and ranks per AP as well as globally ([Siril multipoint docs](https://siril.readthedocs.io/en/latest/preprocessing/multipoint.html)) [V].
- **Solar professional**: Denker et al. (2018) recommend MFGS for a priori quality assessment of large HiFI archives, and show that a modified MFGS reveals field and structure dependence of the metric ([arXiv:1802.00760](https://arxiv.org/abs/1802.00760)) [V abstract].

### 3.2 Which correlates best with true seeing?

Honest answer: **I could not find a peer-reviewed study that compares these metrics against a known-truth seeing measure on solar data.** The nearest items:

- Denker et al. (2018) compare metrics on HiFI data and recommend MFGS, but the abstract does not name the alternatives or give rank correlations [V abstract; full text not read].
- The hybrid lucky-imaging paper [arXiv:2012.05480](https://arxiv.org/pdf/2012.05480) scores *output images* with seven measures including Tenengrad and Laplacian, which tests output sharpness metrics, not frame-selection metrics [V, snippet].
- General focus-measure comparisons (e.g. Groen et al. 1985, Cytometry; Pech-Pacheco et al. 2000, ICPR) [M] show that gradient and Laplacian measures behave well on high-SNR data but diverge under noise.
- The review of digital lucky imaging, [arXiv:2411.00338](https://arxiv.org/pdf/2411.00338), lists intensity-variance, Strehl and related measures but gives no ranking [V, snippet].

Theory gives clear guidance:

1. The measure must respond to power at spatial frequencies the *seeing* removes: frequencies between roughly r0/lambda and D/lambda, i.e. the mid-to-high band. Very low frequencies (brightness gradients, limb) are irrelevant; frequencies above the diffraction cutoff are pure noise.
2. Photon and read noise add a positive, frame-independent, but brightness-dependent bias to any energy-type metric. Without correction, a bright noisy frame outranks a dim sharp one. Both Siril (brightness normalisation) and AutoStakkert (noise-robust parameter) address this partly.
3. A band-limited Fourier power metric with an estimated noise floor subtracted is the most defensible: it is the sum over the band of (|F(k)|^2 minus the noise power), and the noise power can be estimated per frame from the power above the diffraction cutoff, where there is no signal. This reduces the bias directly. It is a recommendation from first principles rather than a published comparison: **mark as hypothesis; test in Helia's synthetic harness (Section 12).**
4. Global versus local: the isoplanatic argument (Section 1.4) says that a single global score hides the fact that, within a frame, some patches are sharp and others not. All three open implementations compute both a global rank (for choosing a reference and a coarse cut) and an AP-local rank (for the actual selection). Siril and PSS blend patches and fade frames near the cutoff rather than cut hard (Siril: "frames near the cutoff fade in smoothly") [V].

### 3.3 Recommendation for Helia

| Item | Verdict |
|---|---|
| Gradient energy (Sobel/Tenengrad) with noise-floor subtraction and brightness normalisation | **MVP**: cheap, GPU-trivial, matches AS!3/PSS behaviour. |
| Laplacian std/variance | **MVP** as a second selectable metric for compatibility and comparison. |
| Band-limited Fourier power with per-frame noise floor | **MVP+ / early later**: needs an FFT on GPU anyway; evaluate against gradient on synthetic data first. |
| MFGS | Later: implement for parity with professional archives; cheap (median filter + gradient). |
| Granulation RMS contrast | Report as a diagnostic (frame statistic), not the ranking driver. |
| Global-only selection | Not enough on the Sun; use only for reference-frame choice. |
| Local per-AP selection with soft cutoff | **MVP** (this is the core of multipoint lucky imaging). |

Implementation note: compute all patch metrics from one shared pass over the frame stored as f32 in a GPU buffer; the metric can be reduced per AP by workgroup reduction, no readback needed other than the ranks.

---

## 4. Registration

### 4.1 Models

1. **Global translation**: remove whole-frame motion (dominated by tip/tilt, mount flexure, wind). Needed before any local step so the local search window can be small.
2. **Local translation per AP**: estimate the shift (dx, dy) of each patch relative to a reference; this is a piecewise approximation of the local-tilt field.
3. **Dense warp**: interpolate AP shifts to a smooth displacement field, resample each frame once (destretching).

### 4.2 Estimators

- **Normalised cross-correlation (NCC)**: peak of the correlation of a template with the search window; robust to additive brightness offsets when zero-mean; direct implementation costs O(W^2 S^2) per AP for template size W and search range S, which is small for AP work (W = 32 to 64, S = ±8) and parallelises naturally on a GPU (one workgroup per AP and frame).
- **Phase correlation** (Kuglin & Hines 1975, ICCS) [M]: inverse FFT of the normalised cross-power spectrum; delta-like peak, contrast-independent, but requires windowing to avoid edge leakage and is dominated by high-frequency noise unless band-limited. The PSS `translation` routine is a plain phase correlation returning whole-pixel shifts [V code]; ImPPG's alignment uses phase correlation on high-contrast features or limb stabilisation with optional sub-pixel accuracy ([ImPPG README](https://github.com/GreatAttractor/imppg)) [V].
- **Sub-pixel peak refinement**: (a) biquadratic/paraboloid fit on the 3 x 3 neighbourhood (November & Simon 1988 found a biquadratic surface best in their numerical tests, per [arXiv:0707.1994](https://arxiv.org/pdf/0707.1994) snippet [V, secondary]; PSS `sub_pixel_solve` does exactly this with a precomputed normal-equation matrix and accepts a correction only if within about one pixel [V code]); (b) matrix-multiply DFT upsampling around the coarse peak, Guizar-Sicairos, Thurman & Fienup (2008), ["Efficient subpixel image registration algorithms", Opt. Lett. 33, 156](https://labsites.rochester.edu/fienup/wp-content/uploads/2019/07/MGS_OL08_EffRegistration.pdf) [V], which reaches the accuracy of full FFT upsampling at a small fraction of the cost and memory (their example: 1/20 pixel on 1024 x 1024 would need a 20,480 x 20,480 inverse FFT by brute force). PSS also calls scikit-image `phase_cross_correlation` with `upsample_factor=10` (the "Subpixel" method) [V code].
- **Optical flow** (Lucas-Kanade, Horn-Schunck, or modern dense flow): dense by construction, so it needs no AP grid, but granulation is low-contrast and evolves, which violates brightness constancy; flow is also expensive for thousands of frames. It is a plausible *refinement* after AP-based registration but not an MVP candidate [M]. Local correlation tracking (LCT) used in solar physics (November & Simon 1988; also [FLCT](https://ar5iv.arxiv.org/html/0712.4289), a fast LCT implementation) [V, snippets] is conceptually an AP grid at every pixel, so the two approaches are the same family.

### 4.3 What the existing tools do

| Tool | Global alignment | Local method | Notes |
|---|---|---|---|
| **AutoStakkert!3** | Disc centroid or surface correlation [M] | AP grid, per-AP correlation; local (AP) quality; sub-pixel; drizzle 1.5x and 3x [M] | Closed source, freeware [M]; no official algorithm doc retrieved. |
| **Registax** | Alignment on a chosen reference region; "optimise" stage [M] | Limited local alignment | Largely superseded for alignment; still used for wavelets [M]. |
| **PlanetarySystemStacker** | Rank all frames by global quality, then align globally on a high-structure patch using the chosen method ([README](https://github.com/Rolf-Hempel/PlanetarySystemStacker)) [V] | AP mesh with brightness/contrast/structure rejection; per-AP ranking; multi-level correlation: phase 1 on a half-resolution box with a wide search, phase 2 on full resolution with a small search around the phase-1 result, optional sub-pixel quadratic fit; alternates: "Subpixel", "CrossCorrelation", "RadialSearch", "SteepestDescent" [V code] | Python 3, OpenCV, numpy, Qt5; GPLv3 per the source header [V]. Patch accumulation is in a separate module (not read). |
| **Siril** (multipoint) | Planet mode: brightness centroid; Surface mode: cross-correlation on an auto-picked patch; reference built from best 5 per cent of frames [V] | AP spacing default 2.25 x the correlation half-width (54 px at the default half-width of 24); search width 14 px; zero-mean option; two-pass refinement against a sharper local reference; robust local smoothing across neighbouring APs (default radius 2.5 grid steps); patch engine with raised-cosine blending and brightness matching, and an experimental warp engine that interpolates AP shifts to a single smooth field and resamples each frame once [V] | GPLv3 [M]. Output scale 1x, 1.5x, 2x, 3x; 32-bit float accumulation [V]. |
| **ImPPG** | Phase correlation or solar limb stabilisation, sub-pixel option [V] | None (alignment of sequences, not lucky imaging) | GPLv3, C++, OpenGL [V]. |

Two design lessons are worth copying from Siril: (1) the **refinement pass**, where the best frames at each AP are stacked into a sharper local reference and shifts are re-measured against it (the first reference is blurred, which biases shifts toward the mean); (2) **robust smoothing of the shift field**, which cuts measurement noise while preserving genuine warp.

### 4.4 Destretching in professional solar work

"Destretching" is the solar term for the same operation: a local correlation scheme determines displacements against a reference, then frames are interpolated to match. It was introduced for removal of image distortion (November 1986) and extended to granule tracking by November & Simon (1988), ApJ 333, 427, whose abstract reports that the time-averaged localised cross-correlation gives displacement not biased by atmospheric seeing (as summarised in [arXiv:0707.1994](https://arxiv.org/pdf/0707.1994) and the search results) [V, secondary]. Practical reports use multiple passes with shrinking windows (one workflow used four passes) to correct different spatial scales [V, snippet]. Speckle and MFBD pipelines at SST (SSTRED) and GREGOR do the equivalent inside their sub-field processing, followed by mosaicking of the reconstructed patches ([SSTRED, arXiv:1804.03030](https://arxiv.org/pdf/1804.03030)) [V, snippet].

### 4.5 Recommendation

| Component | Verdict |
|---|---|
| Global translation by windowed, high-pass, band-limited phase correlation, f32 FFT (or the PSS multilevel NCC alternative) | **MVP**. Provide the limb-fit / centroid mode for full-disc frames where a limb is present. |
| AP grid sized in arcseconds, with brightness, contrast and structure rejection; NCC search ±S px; quadratic sub-pixel fit | **MVP**. |
| Multi-pass: coarse (half-resolution) then fine; refinement against a locally stacked reference | **MVP+** (small cost, large benefit; Siril default). |
| Smoothing of the AP shift field by robust local fit | **MVP+**. |
| Dense warp resampling (destretch) rather than patch pasting | **Later** (needs a GPU gather kernel, bicubic or Lanczos resampling; avoids patch seams). Build patch blending first with Hann windows. |
| DFT-upsampled sub-pixel (Guizar-Sicairos) | **Later**, only if the quadratic fit shows pixel-locking bias in synthetic tests (peak-locking is a known failure of parabolic fits [M]). |
| Optical flow | Not worth it for MVP. |

GPU compute estimate (mine): 5000 frames x 400 APs x (32 x 32 template) x (17 x 17 shifts) is about 5.9e11 multiply-adds, i.e. well under a second of ideal arithmetic on a consumer GPU and a few seconds in practice. The AP stage is therefore cheap; I/O and memory bandwidth dominate.

---

## 5. Stacking

### 5.1 Estimators

After registration, each output pixel (or AP patch) is combined from N selected samples:

- **Mean**: optimal (minimum variance, unbiased) under independent Gaussian noise and identical expectation. Preserves the linear photometry that solar users want. Noise falls as 1/sqrt(N). This is the default in PSS and Siril's multipoint stacking (the patch engine averages best frames) [V].
- **Median**: robust to outliers but 25 per cent less efficient than the mean for Gaussian noise (asymptotic variance factor π/2 [M]), and the median of resampled, sharpened frames loses the highest frequencies slightly more than the mean. Rarely used for lucky imaging.
- **Sigma clipping / winsorising**: reject samples more than kσ from a robust centre. Helpful against cosmic-ray-like events (rare in short ground exposures), hot pixels not removed by darks, and frames corrupted by drifting clouds or bird transits. Caution: on a registered stack whose samples are *meant* to differ at the finest scales (seeing-induced differences carry residual signal), clipping at low k can erode genuine bright and dark detail, creating a slight loss of contrast. A conservative k (3 to 4) and iteration limit are standard [M].
- **Quality-weighted mean**: weights w_i derived from rank or metric; Siril fades frames near the cutoff and PSS keeps a fixed top-N per AP [V]. A weight w ∝ (quality)^p, or a smooth taper from 1 to 0 over the last 10 to 20 per cent of the selected set, avoids the hard threshold's discontinuity as seeing changes.

### 5.2 How many frames

Stacking N independent frames reduces noise by sqrt(N), but the gain from seeing selection is in the *signal* (sharper frames) and N falls as the cut tightens. Amateur practice stacks the best 10 to 50 per cent depending on seeing and frame count [M]; the AutoStakkert 50 per cent line is a widely used cutoff, per the community guidance cited in Section 3 [V, snippet]. Helia's UI should show the quality-versus-rank curve and set a default from the knee, but allow per-AP override.

### 5.3 Drizzle

Fruchter & Hook (2002), ["Drizzle: A Method for the Linear Reconstruction of Undersampled Images", PASP 114, 144](https://arxiv.org/abs/astro-ph/9808087) [V]: input pixels are shrunk to "drops" (parameter pixfrac) and mapped onto a finer output grid with geometric transformation, preserving photometry and resolution and avoiding further convolution, with per-pixel weights. It was developed for dithered HST data.

When it helps: only if the data are *undersampled* (q = lambda F / (2p) < 1 in Section 2.2) *and* the shifts are diverse in the sub-pixel sense. Lucky imaging satisfies the second condition for free, because seeing jitter provides random sub-pixel dither. For oversampled or Nyquist-sampled solar data drizzle gains nothing but costs noise correlation (adjacent output pixels share input pixels) and holes at small pixfrac with few frames. Siril offers 1x, 1.5x, 2x, 3x output scale, with 32-bit accumulation [V]; PSS has a drizzle factor and sizes its stacking buffer accordingly [V code]. AutoStakkert offers drizzle at 1.5x and 3x [M].

Implementation hint: design the accumulator as "splat each (frame, AP) sample with a position and a weight into a float buffer plus a weight buffer, then divide". Then drizzle is only a different splat kernel (drop footprint) and scale factor, and "mean" with bilinear or Lanczos resampling is another. This makes the MVP drizzle stub a real option rather than a rewrite.

### 5.4 Recommendation

| Method | Verdict |
|---|---|
| Quality-weighted mean with soft cutoff | **MVP default**. |
| Optional sigma clip (k 3 to 4, 1 to 2 iterations), applied to the *registered, normalised* samples | **MVP**, off by default. |
| Median | Later, as an option only. |
| Drizzle (variable pixfrac, scale 1.5x to 3x) | **Later**; enable only when q < 1; keep the splat design now. |

---

## 6. Image restoration at professional observatories

### 6.1 Speckle interferometry

**Labeyrie (1970)** [M] showed that averaging the *power spectrum* of many short exposures preserves high-frequency information (the "speckle transfer function", STF) that long exposures lose; this recovers amplitudes but not phases. To recover phases:

- **Knox-Thompson** (Knox & Thompson 1974, ApJ 193, L45, "Recovery of images from atmospherically degraded short-exposure photographs") [V citation via search] uses the cross-spectrum <I(k) I*(k+Δ)> for small shifts Δ to propagate phase differences.
- **Speckle masking / bispectrum** (Weigelt 1977, Opt. Commun.; Lohmann, Weigelt & Wirnitzer 1983, Appl. Opt.) [V citation via search] uses the triple correlation / bispectrum <I(k1) I(k2) I*(k1+k2)>, which is insensitive to the random atmospheric phase and gives absolute phase information, at much higher computational and memory cost.
- **Solar adaptation**: von der Lühe (1993), "Speckle imaging of solar small-scale structure. I. Methods", A&A 268, 374, and Paper II (A&A 281, 889, 1994) [V citation via search] adapted speckle masking to the extended Sun, working on sub-fields within the isoplanatic patch; there is no calibration star on the Sun, so the STF must be *modelled* from an estimated r0 (see the 2003 DOT discussion referenced at [robrutten.nl](https://robrutten.nl/rrweb/rjr-pubs/2003ASSL..288..111R.pdf), [V snippet]).
- **KISIP**: Wöger & von der Lühe (2008), "KISIP: a software package for speckle interferometry of adaptive optics corrected solar data", SPIE 7019, 70191E; and Wöger, von der Lühe & Reardon (2008), ["Speckle interferometry with adaptive optics corrected solar data", A&A 488, 375](https://www.aanda.org/10.1051/0004-6361:200809894) [V citations; abstract content via search]. KISIP uses Fourier phase reconstruction by an extended Knox-Thompson or a triple-correlation scheme, splits images into sub-frames sized to the isoplanatic patch (so it parallelises), and models the AO system with wavefront-sensor information. The A&A paper compares DST ground-based reconstructions with Hinode/SOT and concludes that near real-time reconstruction with high photometric accuracy is possible [V].

Evidence of achievement: speckle reconstruction of GREGOR HiFI bursts yields nearly diffraction-limited images ([Denker et al. 2018](https://arxiv.org/abs/1802.00760) [V]). A cautionary data point: Uitenbroek, Tritschler & Rimmele compared the rms contrast of speckle-reconstructed G-band images with simulations and found 14.1 per cent for the best reconstructions versus 21.5 per cent for a simulated quiet Sun, a considerable discrepancy (summarised in the search for HiFI metrics) [V, snippet; the primary paper is "The discrepancy in G-band contrast: where is the quiet Sun?", [arXiv:0704.3637](https://ar5iv.arxiv.org/html/0704.3637)]. So even professional restorations do not recover full contrast, partly because of residual PSF mismatch and partly because of telescope PSF.

### 6.2 Multi-Frame Blind Deconvolution and MOMFBD

MFBD estimates the object and the per-frame PSFs jointly by maximising a likelihood, with PSFs *parametrised by wavefront expansion coefficients* (Zernike or Karhunen-Loève modes) so that every PSF is physically constrained (pupil, diffraction, positivity). Phase diversity (Gonsalves 1982 [M]; Löfdahl & Scharmer 1994 for solar use, cited in [arXiv:2505.10639](https://arxiv.org/html/2505.10639v1) [V, snippet]) adds a deliberately defocused channel to remove the object-PSF ambiguity. **Multi-Object MFBD** (van Noort, Rouppe van der Voort & Löfdahl 2005, Solar Physics 228, 191, "Solar image restoration by use of multi-frame blind de-convolution with multiple objects and phase diversity") [M for volume and page] generalises to several simultaneous *objects* (wavelengths or polarisation states) that share the same wavefront.

Löfdahl (2002), ["Multi-frame blind deconvolution with linear equality constraints", SPIE](https://arxiv.org/abs/physics/0209004) [V], formulates phase-diverse speckle as MFBD plus linear equality constraints on the wavefront parameters, so one code handles several acquisition schemes. Key facts from recent literature [V, snippets]:

- Restoration is performed in sub-fields of roughly 4 to 5 arcsec ([arXiv:2205.13650](https://arxiv.org/pdf/2205.13650v1)), because the space-invariance assumption holds only inside an isoplanatic patch.
- The method is used with CRISP and CHROMIS data from the SST through the SSTRED pipeline ([arXiv:1804.03030](https://arxiv.org/pdf/1804.03030)).
- In low-SNR regimes the joint point estimate can be ill-posed and needs many images ([Marginal MFBD, arXiv:2605.11980](https://arxiv.org/pdf/2605.11980)), which motivates marginalised formulations.
- A modern open-source implementation is **torchmfbd**, a flexible MOMFBD code in PyTorch ([arXiv:2505.10639](https://arxiv.org/pdf/2505.10639)) [V, snippet]. It is the best available reference for how to structure a GPU MFBD, including how to handle objects and modes.

Compute cost. Per iteration, each frame in a sub-field needs forward and backward transforms (about 4 FFTs) plus pointwise work. For a 64 x 64 sub-field the FFT is about 5 N log2 N = 5 x 4096 x 12 ≈ 0.25 MFLOP, so roughly 1 MFLOP per frame per iteration; 64 frames x 200 iterations ≈ 13 GFLOP per sub-field. A 1500 x 1500 px ROI with 32 px sub-field stride has ~2,200 overlapping sub-fields: about 30 TFLOP in total. On a consumer GPU sustaining 1 to 3 TFLOP/s (realistic with small FFTs and memory-bound steps) this is tens of seconds to a few minutes (my estimate; real optimisers also need line searches and the parameter gradient, so allow a factor of two to five). Memory is modest. In summary MFBD is *feasible* on a consumer GPU, but not trivial: it needs a batched small-FFT library (wgpu does not ship one, so Helia would write radix-2/4 kernels or use shared-memory Stockham FFTs; I did not find a mature wgpu FFT crate and did not verify this [unverified]) and an L-BFGS or conjugate-gradient driver.

### 6.3 What adaptive optics adds

AO removes low-order aberrations in real time, increasing the fraction of good frames and relaxing the burden on post-processing; but daytime AO corrects only inside the isoplanatic patch (Section 1.4), so post-facto reconstruction is still used (A&A 2017 statement above) [V snippet]. The DST uses a 97-actuator, 76-sub-aperture correlating Shack-Hartmann system and tested multi-conjugate AO using solar structure as guide targets ([ao4elt abstract](https://ao4elt.lesia.obspm.fr/abstract_120.html)) [V, snippet]; BBSO runs three deformable mirrors over a 35 arcsec wavefront-sensing field ([BBSO AFOSR 2017](https://bbso.njit.edu/pub/staff/pgoode/vayur/afosr_ao_2017.pdf)) [V, snippet]. Amateurs do not have AO on solar telescopes of significance (a few commercial tip-tilt systems exist; I did not verify any), so Helia should treat the problem as post-facto.

### 6.4 Feasibility for an amateur tool on a consumer GPU, and what amateurs have achieved

I looked for documented amateur speckle masking or MFBD on solar video and **found none** (the search returned only professional literature). Consequently:

- Treat the *simplified* versions as research: a **per-sub-field MFBD with a low number of Zernike modes** (say tip, tilt, defocus, astigmatism, coma, spherical: about 10 to 20 modes, using the known telescope pupil including central obstruction) and a Poisson or Gaussian likelihood is the most realistic route, mirroring torchmfbd and SSTRED in miniature.
- A **simplified speckle** path is possible via Knox-Thompson (cross-spectrum, O(N frames x shifts) per sub-field) with an STF model from an r0 estimate. Bispectrum/speckle masking is far more memory and compute hungry, and in the solar setting also needs the STF model; not justified for an MVP.
- Amateurs' realised gain from lucky imaging, stacking and wavelet or Lucy-Richardson sharpening is well established; beyond that point I have no verified evidence. The prudent claim for Helia is: "MFBD may add the equivalent of a larger effective aperture for the best sub-fields, as it does professionally, but proof must come from Helia's own synthetic and real tests."

Failure modes (all professional pipelines): ill-posed with too few frames or low SNR; PSF mis-modelling creates ringing and spurious fine structure; sub-field boundary artefacts (mitigated by overlap and apodisation); sensitivity to flat-field errors; high cost of tuning; granulation contrast mismatch with simulations (Uitenbroek et al.).

### 6.5 Recommendation

| Method | Verdict |
|---|---|
| Speckle masking / bispectrum | Not worth it for Helia. |
| Knox-Thompson speckle | Not worth it unless the MFBD path fails; it is dominated by MFBD in flexibility. |
| Phase diversity | Not applicable to amateur video (no diverse channel); possible later with a deliberate defocus-pair capture, but low demand. |
| Per-sub-field MFBD with a Zernike-parametrised PSF | **Research milestone (later)**: build on CPU reference first, then GPU; validate on synthetic frozen-turbulence data with known truth; ship behind an experimental flag. |
| Shift-and-add with local lucky selection | **MVP**: this is the practical equivalent that delivers most of the gain for typical amateur apertures. |

---

## 7. Post-processing

### 7.1 Wavelets (à trous / Registax-style)

The à trous ("with holes") algorithm is an undecimated wavelet transform using a B3 spline scaling function; in 1D the smoothing mask is (1, 4, 6, 4, 1)/16 and in 2D a 5 x 5 mask, with dilation by inserting holes at each scale; reconstruction is the plain sum of the wavelet planes and the residual ([arXiv:astro-ph/0202107](https://arxiv.org/pdf/astro-ph/0202107); [arXiv:astro-ph/0407053](https://arxiv.org/pdf/astro-ph/0407053)) [V, snippets]. The standard reference is Starck, Murtagh & Bijaoui (1998), *Image Processing and Data Analysis: The Multiscale Approach*, CUP [V via ImageJ plugin references]. The transform is redundant and noise variance differs per scale ([arXiv:astro-ph/0303246](https://arxiv.org/pdf/astro-ph/0303246)) [V, snippet]. Registax-style sharpening multiplies each wavelet plane by a user gain and optionally applies noise thresholding ("denoise"); the exact Registax layer definitions I could not verify [M].

Properties for Helia: each scale is a separable 5-tap convolution applied with stride 2^j, which maps trivially to compute shaders; there is no ringing from the transform itself, but boosting fine scales boosts noise, so a per-scale threshold derived from the MAD of the finest scale is needed. It is a linear unsharp-mask family tool: it cannot recover frequencies the PSF has removed entirely, only reweight them.

### 7.2 Richardson-Lucy (RL) and regularisation

RL (Richardson 1972, JOSA; Lucy 1974, AJ) [M] is the iteration x_{k+1} = x_k · [(y / (H x_k)) ⊛ H^T]; it converges to the maximum-likelihood solution for Poisson noise, preserves flux and non-negativity, but amplifies noise and produces ringing as iterations grow (acceptable solutions "at the cost of hundreds or thousands of iterations" for low noise, per the ADASS summary in the search [V, snippet]; RL-type methods did better on stars than on planet-like extended objects in a 1998 comparison [V, snippet]). Regularised variants:

- **Total-variation RL**: Dey, Blanc-Féraud, Zimmer et al. (2006), "Richardson-Lucy algorithm with total variation regularization for 3D confocal microscope deconvolution", Microsc. Res. Tech. 69, 260 (PMID 16586486) [V]: a TV penalty suppresses oscillations while preserving edges; an ImageJ operator exists, with a regularisation factor of 0.002 used in tutorials [V, snippet]. Parameter selection remains manual (Laasmaa et al. 2011 propose an automatic estimate [V, snippet]).
- **Damped RL** (White 1994) [M]: ignores updates in pixels where residual is consistent with noise.
- **Early stopping**: the simplest regulariser.

Wiener filtering applies H*/(|H|^2 + NSR) in the Fourier domain; fast and linear but yields ringing at sharp edges and requires an NSR estimate. Fine for a quick preview.

**Blind deconvolution** on a single stacked image is under-determined (Section 6.2); for the stacked solar image it is reasonable to use a *non-blind* RL with a PSF modelled as (diffraction MTF of the telescope x residual seeing/registration blur x pixel/aperture smoothing). The stack's effective PSF is a Gaussian-like blur of width tied to the residual seeing; ImPPG's choice of a Gaussian kernel with user sigma is a good-enough approximation ([ImPPG](https://github.com/GreatAttractor/imppg) [V]).

### 7.3 ImPPG's approach

Pipeline: brightness normalisation, Lucy-Richardson with Gaussian kernel (adjustable sigma and iteration count, plus an *experimental option to reduce ringing around bright areas*), unsharp masking (several masks, adaptive mode that varies strength with brightness), tone curve; 32-bit float arithmetic throughout; OpenGL GPU back end that is "often several times faster" than CPU and gives a live preview; batch and Lua scripting; GPLv3; version 2.1.0 dated 2025-11-18 ([README](https://github.com/GreatAttractor/imppg)) [V]. The live-preview architecture is the major usability feature Helia should match or improve on.

### 7.4 Limb, ringing and edge artefacts

The solar limb is a step from bright disc to dark sky; unconstrained RL/wavelet sharpening produces bright and dark rings around it and, for H-alpha, around prominences. Mitigations:

1. Process disc and off-limb separately (see Section 8.2) or use a mask-weighted RL that excludes the sky region from the data term.
2. Extend the disc by reflecting or by extrapolating the limb-darkening profile before deconvolution (boundary handling), and crop afterwards.
3. Use TV or damped RL and cap iterations.
4. Apply the sharpening step to a limb-darkening-flattened image (Section 8.3); this reduces the large-scale gradient that otherwise drives ringing.
5. Wavelet noise thresholds on the finest scales; the 'ringing reduction' options in ImPPG are the baseline to compare against [V].

### 7.5 Noise handling

After a stack of N frames the noise is approximately Poisson-Gaussian; Anscombe or generalised Anscombe variance stabilisation followed by Gaussian denoising (wavelet thresholding, BM3D [M]) is the classical route. For a scientific tool the safest default is denoising *only via the wavelet threshold tied to a measured noise level*, with an explicit user switch, and no non-linear patch denoiser by default.

### 7.6 Recommendation

| Stage | Verdict |
|---|---|
| Unsharp mask, tone curve, brightness normalisation | **MVP** (trivial, GPU). |
| à trous wavelet sharpening with per-scale gains and noise threshold | **MVP**. |
| Non-blind RL with Gaussian (and Airy-plus-Gaussian) PSF, damping and iteration cap | **MVP+**, real-time on GPU. |
| TV-regularised RL | Later. |
| Wiener | Later; optional. |
| Blind single-image deconvolution | Not worth it. |
| Limb-aware masked processing | **MVP+** (needed for solar). |

---

## 8. Solar-specific workflow

### 8.1 Wavelength regimes

| Regime | Target | Practical notes |
|---|---|---|
| White light (continuum, ND or Herschel wedge, 500 to 600 nm) | granulation, sunspots, faculae | high photon flux, short exposures easy; granulation contrast a few per cent; limb darkening strong. |
| H-alpha (656.3 nm, narrowband etalon) | chromosphere, filaments, prominences, plage | photon-limited, etalon sweet spot and rings (Section 2.3); thin field-dependent bandpass; chromospheric structure is high contrast but soft. |
| Ca II K (393.4 nm) | plage, network | r0 at 393 nm is about 0.54 of its value at 656 nm by the lambda^(6/5) scaling (my arithmetic), so seeing is worse; diffraction limit is better; need UV-sensitive sensor and glass choices. |

### 8.2 Prominences versus disc: exposure and compositing

H-alpha disc and off-limb prominences differ in brightness by more than an order of magnitude [M]. Standard amateur practice: take separate captures at different exposure (or process separate stacks with different tone curves), then blend with a limb mask. Helia should (a) allow two exposure sets per scene, (b) detect the limb by circle fit, (c) provide a feathered radial mask (width in arcsec) and (d) stack and sharpen each separately before compositing in linear float, then apply a single tone mapping. Processing in linear light before tone curves is essential to avoid halos.

### 8.3 Limb-darkening correction

For white light, Neckel & Labs (1994), "The solar radiation between 3300 and 12500 A" (Solar Physics 153, 91): they measured the centre-to-limb variation at 30 continuum wavelengths from 303 to 1099 nm (observations at Kitt Peak, 1986 to 1987), fitted by fifth-order polynomials in mu = cos(theta), with data within 7 arcsec of the limb excluded because of seeing, which corresponds to mu of about 0.12 ([summarised in arXiv search results](https://arxiv.org/pdf/2310.05652)) [V, snippet; coefficient tables not retrieved]. Later work found about 0.4 per cent accuracy and possible over-correction for scattered light [V, snippet]. For H-alpha and Ca II K there is no clean closed form for the user's instrument (etalon bandpass and filter profile change the centre-to-limb behaviour), so Helia should fit an *empirical radial profile* to the image (azimuthally median-averaged, spline) and divide by it, with the fit mask excluding active regions.

### 8.4 Colourisation and inversion

There is no standard "true colour" for monochrome H-alpha; conventions are orange or red gradients applied to a mono image, or inverted greyscale (dark disc prominences become bright) used to show prominences and filaments. Helia should keep the processed result as *scientific mono float* and apply colour as a LUT in an export step, with the LUT declared in metadata, so the process remains reversible and scientific data stay uncoloured.

### 8.5 Derotation and animation

The Sun rotates differentially; the Snodgrass & Ulrich (1990, ApJ 351, 309) sidereal rate is approximately 14.713 - 2.396 sin^2(φ) - 1.787 sin^4(φ) degrees per day [M, confirm coefficients]. My arithmetic for the equator at disc centre: 14.713 deg/day is 0.01022 deg/min or 1.78e-4 rad/min, and times a solar radius of about 960 arcsec that is roughly 0.17 arcsec per minute. A 10 minute capture therefore moves features by about 1.7 arcsec at disc centre (more of the disc's apparent motion is foreshortened near the limb), comparable to the seeing, so rotation matters only for captures of several minutes at high sampling. Helia should therefore stack bursts of at most a few minutes unless it derotates by projecting to heliographic coordinates. Derotation of mosaic panels and time-lapse is a standard operation in amateur tools (WinJUPOS for planets and Sun, which is closed source and Windows-only [M]). This needs the position angle (P), B0, L0 from ephemeris (can use an SPICE or low-precision formulae; a mature Rust ephemeris crate was not verified [unverified]).

Time-lapse: align frames to a limb-fit circle, apply a consistent tone map, and optionally derotate; export through ffmpeg. Keep this later.

### 8.6 Full-disc mosaics

A full disc at fine sampling needs panels. A pipeline: capture overlapping panels (20 to 30 per cent), stack each, flatten each radially, estimate pairwise translations in the overlaps (phase correlation; includes small rotation/scale from lens distortions as a refinement), solve a global least-squares for panel positions, blend with feathering or multi-band (Burt-Adelson [M]) in linear space. Alternatives to Microsoft ICE (which I recall as discontinued by Microsoft [M, unverified]) are Hugin with Enblend/Enfuse (GPL, cross-platform), PTGui (commercial), and Siril's own mosaic features. Solar disc mosaics have an easy geometry: the limb provides global constraints, so a limb-circle prior can regularise panel positions. Evolution of the Sun between panels (minutes) is the main inconsistency, which argues for fast panel acquisition and a consistent seeing selection per panel.

---

## 9. Machine learning

### 9.1 Published solar restoration networks

- **Enhance**, Díaz Baso & Asensio Ramos (2018), ["Enhancing SDO/HMI images using deep learning", A&A 614, A5](https://ar5iv.arxiv.org/html/1706.02933) [V]: two fully convolutional residual networks (about 450k parameters; a 40k lite version) for continuum and magnetograms, trained on MURaM simulations degraded by an HMI PSF, to deconvolve and 2x super-resolve. Granulation contrast rose from ≈3.7 to ≈7 per cent on real data compared to degraded Hinode; 400 x 400 images at about 100 ms on GPU. The authors state the result "is a clear indication that Enhance is introducing prior information not present in the data": the residual after re-convolving the output with the PSF is about 4 per cent compared with under 1 per cent for RL; performance is poorer near the limb; training used one simulation. This is a space-based, known-PSF problem; it does not transfer to variable seeing.
- **Real-time multiframe blind deconvolution**, Asensio Ramos, de la Cruz Rodríguez & Pastor Yabar (2018), ["Real-time, multiframe, blind deconvolution of solar images", A&A 620, A73](https://ar5iv.labs.arxiv.org/html/1806.07150) [V]: an encoder-decoder with a fixed 7-frame input (3.15M parameters) and a recurrent variant (4.02M) trained on 80,000 patches of 88 x 88 pixels using **MOMFBD reconstructions of SST/CRISP data as targets**; inference under 5 ms per 1k x 1k image on a Titan X; outputs resemble MOMFBD but are slightly less sharp for fine Ca II 8542 fibrils; **shuffling inputs yields different artefacts and can create spurious polarisation signals**; one telescope, one instrument, one day; hallucination not directly tested [V]. The training target is itself an estimate, so the network inherits MOMFBD's biases.
- **Neural blind deconvolution**, Schirninger, Jarolim, Veronig, Rempel & Wöger (2026), ["Neural blind deconvolution to reconstruct high-resolution ground-based solar observations", arXiv:2603.05033](https://arxiv.org/abs/2603.05033) [V abstract]: a physics-informed coordinate network (implicit neural representation) jointly fitting object and PSFs from a burst, trained per scene; tested on synthetic MURaM data and real GREGOR and DKIST bursts; claims small-scale features beyond state-of-the-art speckle and MFBD; the abstract implies spatially varying PSFs are future work. The "unprecedented" claim is the authors' own and I did not read the full comparison.

### 9.2 Hallucination and the scientific stance

A prior-trained network fills in plausible detail. For a scientific tool the risks are: (1) detail that looks like granulation but is invented (Enhance's own admission above); (2) systematic flux and contrast changes; (3) generalisation failure under unseen seeing, wavelength or instrument; (4) false features near the limb. Mitigations: never overwrite the data; output as a separate layer with a visible badge and metadata; compute the *data-consistency residual* (re-blur the output with the estimated PSF and compare to the input stack; large residual means the network added information); require input-shuffling consistency (as the RT-MFBD authors did, averaging 20 shufflings halved noise variance [V]); and test with **injected features** absent from the input to see whether the model hallucinates them (Section 12).

### 9.3 Runtime options: ONNX Runtime and Coral Edge TPU

- **Coral Edge TPU**: full acceleration needs 8-bit quantised tensors with fixed shapes known at compile time; tensors should be 1D, 2D or 3D; only supported ops are accelerated, and the compiler cannot partition more than once, so after the first unsupported op *everything* runs on the CPU ([Coral model requirements](https://www.coral.ai/docs/edgetpu/models-intro)) [V]. A reported example: UpSampling2D unsupported, replaced by a bilinear resize op [V, snippet]. A frame-quality classifier (small CNN on 64 x 64 patches, int8) fits well; restoration networks with upsampling, 4D tensors, large inputs and wide dynamic range (solar contrast of a few per cent over 12 bits) fit badly because 8-bit weights and activations lose small-contrast information.
- **ONNX Runtime**: supports CPU, CUDA and DirectML providers and a WebGPU execution provider [M, check current status]; in Rust it is used through the `ort` crate, and there are pure-Rust alternatives such as `tract` and `burn` (which has a wgpu backend) [M, unverified]. The cleanest design for Helia is to keep the core pipeline model-free, and add an *optional* plug-in that runs an ONNX model on patches, so the main wgpu pipeline remains deterministic.

### 9.4 Frame-quality classifiers

A learned frame-quality scorer can use the synthetic harness (Section 12) for supervision, with the known per-frame Strehl or sharpness as label. Risk is low because it only reorders frames and never writes pixel values, so it is the *safest ML use*. Evidence that it beats a band-limited Fourier metric: none found. Treat as a later experiment.

### 9.5 Recommendation

| Use | Verdict |
|---|---|
| Learned frame-quality scorer (small CNN, ONNX) | Later, experimental. |
| Learned denoiser on the final stack | Later, optional, clearly labelled. |
| Learned deblurring/super-resolution of the stack | Not for MVP. If added, only as a separate output with data-consistency residual and a hallucination test. |
| Neural MFBD (RT-MFBD style) | Not worth it for Helia: needs MOMFBD training targets and calibrated instrument data. |
| Coral Edge TPU | Not worth it, except possibly for a tiny classifier. |

---

## 10. Existing tools comparison

Licence and platform data not verified in this session are tagged [M]. Where I only know a tool from background knowledge I say so.

| Tool | Licence | Platforms | Strengths | Gaps Helia can fill |
|---|---|---|---|---|
| **AutoStakkert!3** | Freeware, closed source [M] | Windows (works under Wine) [M] | Fast, robust AP-based stacking, local AP quality, noise-robust gradient metrics, drizzle, de-rotation of planets [M]; the de facto standard | Closed; Windows-only; limited solar-specific tools (no limb-aware processing, no flats built in for solar rings); no transparent algorithm description |
| **Registax 6** | Freeware, closed source [M] | Windows [M] | Wavelets sharpening; long-established | Old alignment, no modern multipoint; Windows only |
| **PlanetarySystemStacker** | GPLv3 (source header) [V] | Python 3 with Qt5; Windows, Linux, macOS [V partial] | Open, multilevel correlation, AP rejection, per-AP ranking, optional sharpening, drizzle option [V] | Pure-Python/OpenCV speed; planetary focus; GUI heavy; no solar-specific limb handling |
| **Siril** (multipoint) | GPLv3 [M] | Windows, Linux, macOS | AP engine with refinement, smoothing, warp engine (experimental), SER support, scripting, 32-bit pipeline [V] | Primarily deep-sky workflow; multipoint is new; no solar flat/limb tools; CPU-oriented; no deconvolution tuned for solar [M] |
| **ImPPG** | GPLv3 [V] | Windows, Linux, macOS (OpenGL) [V partial] | Interactive GPU RL + unsharp + tone curve; sequence alignment by phase correlation or solar limb; batch; Lua scripting [V] | Not a stacker; Gaussian-only PSF; limited limb-ringing control |
| **AstroSurface** | Freeware [M] | Windows [M] | All-in-one: stack, wavelets, RL [M] | Closed source; unverified details |
| **FireCapture** | Proprietary, free (aggregator listing) [V, secondary] | Windows, macOS, Linux, Raspberry Pi [V, secondary] | Capture (with SER), planetary and solar ROI, plugins | Capture only |
| **SharpCap** | Mostly free, Pro paid (about £14 per year per FAQ) [V] | Windows | Capture, live flats and live stacking [V] | Capture-side; Windows only |
| **KISIP / SSTRED / MOMFBD** | Professional; availability per observatory; MOMFBD code released by Löfdahl [M]; torchmfbd open (PyTorch) [V] | Linux / IDL / C++ / Python | Speckle and MFBD at professional quality | Not packaged for amateurs; no GUI; calibration-heavy |

Gaps in the open landscape that Helia can address, by priority:

1. **Solar-specific stack**: limb-aware alignment and flats, limb-darkening and radial flattening, and prominence compositing as first-class features.
2. **GPU throughput** for the AP stage and live parameter preview (only ImPPG currently uses a GPU, and only for sharpening).
3. **Transparent, reproducible** algorithms with deterministic CPU reference (PSS has it in Python, but slowly).
4. **Metrics and diagnostics**: quality-versus-time plots, per-AP stack-depth maps, shift-field maps (none of the free tools expose these well).
5. **Optional research-grade restoration** (per-sub-field MFBD) behind a flag.

---

## 11. Recommended Helia pipeline

### 11.1 Ordered stages

**MVP (milestone M1)**

| # | Stage | Method | Reason |
|---|---|---|---|
| 1 | Read | SER (v2/v3, defensive header and endianness), FITS cube; timestamps | The native astro capture format; timestamps support seeing diagnostics and later derotation. |
| 2 | Calibrate | Master dark (optional) and flat (solar-aware: allow flats captured at the same focus); hot-pixel map; convert to f32 | Dust and etalon rings are fixed in the sensor frame while the image moves; calibration prevents stacking a fixed pattern into detail. |
| 3 | Frame quality (global) | Band-limited gradient energy with noise-floor subtraction and brightness normalisation; Laplacian std as an alternative | Cheap, GPU-trivial, mirrors AS!3/PSS/Siril; only for reference choice and coarse cut. |
| 4 | Global alignment | Windowed, high-pass phase correlation (FFT), sub-pixel via quadratic fit; limb-fit option for full-disc frames | The tip/tilt dominates the variance (Section 1.2); phase correlation is contrast-independent; limb stabilisation exists in ImPPG. |
| 5 | AP grid | Spacing and box in arcsec tied to isoplanatic scale (default 5 to 10 arcsec box); rejection by brightness, contrast, structure; 50 per cent overlap | Section 1.4; same rejection logic as PSS and Siril. |
| 6 | Local quality and selection | Per-AP metric as stage 3 at patch scale, rank frames, soft cutoff | The isoplanatic argument; fade at the cutoff as Siril does. |
| 7 | Local shifts | NCC search ±S px, coarse-to-fine, quadratic sub-pixel | Cheap on GPU (Section 4.5), PSS and Siril precedent. |
| 8 | Stack | Quality-weighted mean of resampled patches, Hann windows, background fill from global stack; optional sigma clip off by default; f32 accumulation | Linear photometry; no seams (Siril patch engine). |
| 9 | Basic sharpen | Unsharp mask, à trous wavelets with per-scale gain and noise threshold; export f32 FITS/16-bit TIFF | Immediate usefulness; compatible with ImPPG for RL. |
| 10 | Diagnostics | Quality curve, AP validity map, shift-field plot, stack-depth map | Fills a gap in tooling; essential for debugging. |

**MVP+ (M2)**

| # | Stage | Method | Reason |
|---|---|---|---|
| 11 | Two-pass AP refinement | Stack best frames per AP, re-measure shifts against sharper local reference; robust smoothing of the shift field | Removes reference bias; Siril default. |
| 12 | Non-blind RL | Gaussian + Airy-based PSF, damping, iteration cap, live preview | ImPPG parity; real-time on GPU. |
| 13 | Limb-aware processing | Radial flat, limb circle fit, masked deconvolution, ringing guard | Solar-specific ringing control. |
| 14 | Quality metric upgrade | Band-limited Fourier power with measured noise floor, selected by synthetic tests | Tests the hypothesis in Section 3.2. |

**Later (M3 and beyond)**

| # | Stage | Method | Reason |
|---|---|---|---|
| 15 | Dense warp / destretch | Smooth displacement field, resample each frame once (bicubic/Lanczos) | Removes patch seams; Siril's experimental warp engine shows feasibility. |
| 16 | Drizzle | Splat kernel with pixfrac, enabled when q < 1 | Only for undersampled data. |
| 17 | Mosaic | Phase-correlation panel registration, limb prior, multi-band blend | Full-disc imaging. |
| 18 | Derotation / time-lapse | Heliographic projection using P, B0, L0 and differential rotation | Long captures and animations. |
| 19 | TV-RL and Wiener | Regularised options | Quality and control. |
| 20 | Per-sub-field MFBD (experimental) | Zernike-parametrised PSFs, L-BFGS, batched small FFTs | Research path; follow torchmfbd/SSTRED structure. |
| 21 | ML plug-ins | ONNX patch models for denoise and frame-quality; labelled outputs | Optional, hallucination-guarded. |

### 11.2 Changes to the current MVP design

The current design is: CPU reference + wgpu backend, SER/FITS input, quality ranking, phase-correlation global alignment, AP-grid multipoint alignment, sigma-clipped stacking, drizzle stub. Findings:

1. **Keep** the CPU-reference plus wgpu approach, SER/FITS input, and the AP grid. They match the practice of every tool reviewed.
2. **Change quality ranking from a single global score to global plus per-AP local ranking**, with a soft taper and brightness normalisation. A single global rank is inadequate on the Sun (Section 1.4). Add noise-floor subtraction to the metric.
3. **Add calibration (flats, darks, hot pixels) to the MVP.** Solar data are dominated by dust and etalon fringes; without flats, de-warping amplifies fixed patterns. This is not in the list above.
4. **Define AP size in arcsec, not pixels**, from the user's optics, and warn about sampling. Add a sampling-ratio calculator.
5. **Phase correlation needs windowing, high-pass whitening and a limb-fit fallback**; do not rely on a naive FFT phase correlation of raw frames. Plan for it to fail on featureless ROIs; return a confidence value.
6. **Make sigma clipping optional and off by default; default to quality-weighted mean.** In seeing-selected stacks the clip can erode fine contrast (Section 5.1). Retain it as an outlier guard.
7. **Re-shape the drizzle stub into a general "splat accumulator"** so that bilinear resampling, Lanczos resampling and drizzle are different kernels of the same accumulator. Activate drizzle only when the sampling ratio indicates undersampling.
8. **Add the refinement pass and shift-field smoothing** as soon as the single pass works (Siril's default behaviour; small cost).
9. **Add a minimal sharpening stage (unsharp and à trous) to the MVP**, plus f32 FITS/TIFF output, so that users can complete a workflow without leaving Helia; defer RL to M2 unless trivial.
10. **FFT strategy**: wgpu has no FFT library; avoid depending on FFT for the AP stage (use direct NCC with small search windows) and write one radix-2/4 Stockham FFT for the global step and for later Fourier metrics. Take care with f32 accumulation (no f64 in baseline WGSL [M]); use pairwise or Kahan summation in the stack.
11. **Determinism**: CPU and GPU results will differ at f32 rounding; define tolerances (Section 12) and test both.

---

## 12. Test methods

### 12.1 Synthetic data with known truth

Build a generator in the repository (Rust or Python tooling) producing sequences where *every* quantity is known:

1. **Truth object**: (a) a granulation-like field made by filtering random noise to a power-law spectrum with Voronoi cells (cheap); (b) a radiative-MHD snapshot if available (MURaM is used in [Díaz Baso 2018] and [Schirninger 2026]; I did not verify whether public snapshots can be redistributed) [V for use, unverified for availability]; (c) a limb disc with a Neckel-Labs profile and sharp dark sky for ringing tests; (d) a bar target and Siemens star for MTF.
2. **Turbulence**: phase screens by the FFT method with a Kolmogorov or von Kármán spectrum (Φ(κ) ∝ r0^(-5/3) κ^(-11/3) [M]) with chosen r0 (3 to 10 cm), frozen flow (wind translates the screen between frames), and several layers or a ground-layer screen with height so that the PSF varies across the field: **anisoplanatism is simulated by shifting the screen by an amount proportional to field angle** [M]. The truth tilt field is recorded for every frame.
3. **Imaging**: PSF = |FT(pupil · exp(i φ))|^2 with a pupil including central obstruction if desired; convolve (space-variant by patches with overlap) with the truth object; resample to a camera pixel grid at a chosen q (undersampled, Nyquist, oversampled); add Poisson and read noise; quantise to 8/12/16 bits; apply global jitter.
4. **Instrument defects**: dust shadows, Newton-ring-like fringes, hot pixels, brightness drift, vignetting, so that calibration and robust rejection have a truth.
5. **Export** as SER with timestamps so the whole reading path is tested.

### 12.2 Per-stage measurements

| Stage | Measure | Pass criterion (initial proposal, tune on data) |
|---|---|---|
| Reader | Round-trip bit-exactness against the generator's stored frames; fuzz of malformed headers | 0 differences; no panic on malformed input |
| Calibration | Residual correlation of the output with the injected dust/fringe pattern | At least 20 dB reduction |
| Quality metric | Spearman rank correlation between metric and truth per-frame (and per-AP) Strehl or sharpness, across r0 and noise levels; also fraction of the truth-best 10 per cent captured by the metric's top 10 per cent | Report for each metric; choose the highest and most noise-stable |
| Global alignment | RMS error vs truth tilt after removing the field mean | Below 0.1 px at SNR where visible features exist; reports low confidence at featureless ROIs |
| AP shifts | RMS error of the recovered shift field vs the truth tilt field; failure rate | Below 0.15 px; failure below 2 per cent |
| Stack | PSNR/SSIM vs the truth object (convolved with the diffraction-limited PSF, which is the best possible outcome), azimuthally averaged power-spectrum ratio, granulation rms contrast vs truth, flux conservation | Monotonic improvement with N and selection; contrast ratio approaching diffraction-limited value; flux error below 0.5 per cent |
| Sigma clip | Bias in mean and contrast with and without clipping on clean data | Contrast loss below 1 per cent at default k |
| Drizzle | Resolution gain (MTF at the Nyquist of the original grid) for q < 1; noise correlation | Gain only when undersampled; documented noise penalty |
| Sharpening/RL | MTF recovery; ringing amplitude at the synthetic limb and at bar targets (overshoot percentage); noise gain; reproducibility of flux | Overshoot below a stated threshold (e.g. 2 per cent); no negative pixels |
| Mosaic | Seam residual and position error vs truth shifts | Below 0.3 px; no visible seams |
| Derotation | Feature position error after derotation vs the synthetic rotation | Below 0.5 px |
| MFBD (if built) | Recovery of injected Zernike coefficients; improvement over the stack; failure on mismatched PSF | Coefficient error below 20 per cent of the injected value |
| ML (if built) | Injected-feature test: add a feature to the truth that is absent from inputs; confirm the model does not create it; data-consistency residual | No hallucinated feature; residual reported with every ML output |

### 12.3 CPU versus GPU parity and performance

- Run each kernel on the CPU reference and wgpu on identical inputs; compare with a stated tolerance (for example max absolute difference of 1e-4 relative for f32 stacks; ranks may swap near ties, so compare rank-correlation not equality).
- Record throughput (frames/s per stage) and peak memory; a regression test with a fixed 5000-frame synthetic capture.
- Run tests on multiple GPUs and the software (llvmpipe/WARP) adapter to expose backend-specific bugs [M].

### 12.4 Real captures

1. **Self-consistency**: split-half stacks (odd and even frames, or two independent selections) and **Fourier Ring Correlation** between them as a resolution estimate with no ground truth (FRC; Saxton & Baumeister 1982, van Heel 2005) [M]; the resolution at which FRC drops below 0.143 or 0.5 gives a defensible, comparable figure, and the stability of FRC is a test of the whole pipeline.
2. **Cross-tool comparison**: stack the same SER with Helia, AutoStakkert!, PSS and Siril on identical selection fractions; compare granulation rms contrast, power spectra, and subjective detail; publish SER files and settings.
3. **Cross-instrument comparison**: compare against co-temporal Hinode/SOT or SDO/HMI continuum images for white-light active regions (as the Enhance and speckle papers did with Hinode [V]); after reprojection and PSF-matching the correlation coefficient and contrast spectra can be computed. Data availability and licensing of such products should be confirmed before building tests on them [unverified].
4. **Public professional data**: GREGOR HiFI and DKIST bursts have been used in published reconstructions ([Schirninger 2026](https://arxiv.org/abs/2603.05033)); whether bursts are open for redistribution in a test suite I have not verified.
5. **Regression gallery**: a small set of real SER captures (white light, H-alpha, Ca II K) of varied seeing, with recorded settings, as golden outputs checked in as hashes plus tolerance, reviewed visually by a person before any golden update.
6. **Seeing diagnostics**: plot the quality metric against time per capture; compare the autocorrelation time with the 50 Hz+ guidance (Section 1.5) and check that the metric tracks visible blur on hand-picked frames.

---

## 13. Open questions and unverified items

- Daytime r0 and coherence-time tables (Rimmele & Marino 2011) not retrieved; numbers in Section 1 are order-of-magnitude.
- No solar frame-metric comparison with ground truth found (Section 3.2); Denker et al. (2018) full text not read; MFGS authorship and definition are [M].
- No amateur speckle or MFBD results found (Section 6.4).
- Licence and platform data for AutoStakkert!, Registax and AstroSurface unverified; Siril's GPLv3 licence is from background knowledge.
- Whether a mature wgpu FFT crate, Rust ephemeris crate, or ONNX WebGPU execution provider is production-ready was not checked.
- Uitenbroek et al. G-band contrast numbers come from a search snippet and should be confirmed in the paper before quoting.
- Snodgrass & Ulrich coefficients, Neckel-Labs coefficients, Noll 87 per cent tilt figure, the Fried-1978 constants, and Registax's internal wavelet definition should be re-checked at the source.

## 14. Reference list (with check status)

Primary and near-primary sources that were opened or surfaced in this review [V]:

- Asensio Ramos, de la Cruz Rodríguez, Pastor Yabar (2018), Real-time, multiframe, blind deconvolution of solar images, A&A 620, A73. https://ar5iv.labs.arxiv.org/html/1806.07150
- Díaz Baso & Asensio Ramos (2018), Enhancing SDO/HMI images using deep learning, A&A 614, A5. https://ar5iv.labs.arxiv.org/html/1706.02933
- Schirninger, Jarolim, Veronig, Rempel, Wöger (2026), Neural blind deconvolution to reconstruct high-resolution ground-based solar observations. https://arxiv.org/abs/2603.05033
- Denker et al. (2018), Image Quality in High-resolution and High-cadence Solar Imaging, Solar Physics. https://arxiv.org/abs/1802.00760
- Law, Mackay, Baldwin (2006), Lucky imaging: high angular resolution imaging in the visible from the ground, A&A 446, 739. https://arxiv.org/abs/astro-ph/0507299
- Fried (1978), Probability of getting a lucky short-exposure image through turbulence, JOSA 68, 1651. https://doi.org/10.1364/JOSA.68.001651
- Fruchter & Hook (2002), Drizzle, PASP 114, 144. https://arxiv.org/abs/astro-ph/9808087
- Guizar-Sicairos, Thurman, Fienup (2008), Efficient subpixel image registration algorithms, Opt. Lett. 33, 156. https://labsites.rochester.edu/fienup/wp-content/uploads/2019/07/MGS_OL08_EffRegistration.pdf
- Löfdahl (2002), Multi-frame blind deconvolution with linear equality constraints. https://arxiv.org/abs/physics/0209004
- Wöger, von der Lühe, Reardon (2008), Speckle interferometry with adaptive optics corrected solar data, A&A 488, 375. https://www.aanda.org/10.1051/0004-6361:200809894
- Rimmele & Marino (2011), Solar Adaptive Optics, Living Rev. Sol. Phys. 8, 2. https://doi.org/10.12942/lrsp-2011-2
- Dey, Blanc-Féraud, Zimmer et al. (2006), Richardson-Lucy with total variation regularisation, Microsc. Res. Tech. 69, 260 (PMID 16586486).
- Torchmfbd (2025). https://arxiv.org/pdf/2505.10639 ; SSTRED (2018). https://arxiv.org/pdf/1804.03030 ; MFBD with uncorrected high-order modes (2022, authors not verified). https://arxiv.org/pdf/2205.13650; Marginal MFBD (2026). https://arxiv.org/pdf/2605.11980
- Tools: PlanetarySystemStacker https://github.com/Rolf-Hempel/PlanetarySystemStacker ; Siril multipoint https://siril.readthedocs.io/en/latest/preprocessing/multipoint.html ; ImPPG https://github.com/GreatAttractor/imppg ; AutoStakkert! https://www.autostakkert.com ; SER format https://grischa-hahn.homepage.t-online.de/astro/ser/index.htm ; Coral https://www.coral.ai/docs/edgetpu/models-intro

Classical references cited from background knowledge [M]: Fried (1966); Noll (1976); Labeyrie (1970); Knox & Thompson (1974); Weigelt (1977); Lohmann, Weigelt & Wirnitzer (1983); von der Lühe (1993, 1994); van Noort, Rouppe van der Voort & Löfdahl (2005); Gonsalves (1982); Löfdahl & Scharmer (1994); November & Simon (1988); Neckel & Labs (1994); Snodgrass & Ulrich (1990); Richardson (1972); Lucy (1974); White (1994); Kuglin & Hines (1975); Starck, Murtagh & Bijaoui (1998); Saxton & Baumeister (1982); van Heel (2005).
