# Helia: state of the art in solar imaging methods, refresh of October 2026

Status: research refresh, for tracking issue issue #15 ("include all the most up to date processes"). Companion to `docs/research/imaging-methods.md`, which this file does not replace. Audience: Helia designers (GPL-3.0, pure Rust, wgpu).

## 0. How to read this document

Tags:

- **[V]** verified this session: the source page, abstract, changelog, code listing or PDF text was opened and read (WebFetch or a local PDF extraction).
- **[U]** unverified: seen only as a web-search snippet or a secondary summary, or stated from background knowledge. Treat as "probably right, confirm before relying on it".

### Statements about other programs (checked October 2026)

This refresh names other programs only to say what they do, in the nominative sense. Helia is not affiliated with,
endorsed by or sponsored by any of them, and their names belong to their owners. Statements about a living product
were re-checked against its own site or repository in October 2026 and are tagged [V] only then. Licence, price,
platform and speed claims that could not be checked there were removed or reduced to "see the vendor's site", so
this page does not rely on memory for them. Programs change: check the vendor before relying on any line below.

Scope limits, stated plainly. WebFetch summarises pages with a small model, so numbers quoted from fetched pages are as reported by that summary; anything that drives a design decision should be re-read at source before it is acted on. I found **no** 2023 to 2026 paper that compares TV-L1 or learned optical flow against AP-grid destretching on solar seeing data, **no** solar-specific diffusion-model restoration paper, and **no** GPU speckle paper newer than 2020 outside the PyTorch MFBD work. Where a search found nothing, that is said in the section.

"Current" means as of the sources dated below.

---

## 1. Professional pipelines and their latest methods

### 1.1 DKIST (VBI, VTF, data centre)

- VBI reconstruction uses a burst of **80 frames at 30 Hz (2.667 s)**; with mechanism moves each reconstructed image takes 3.4 s; the raw frames are discarded after reconstruction, so Level 0 is never available for speckle-reconstructed VBI data. The NSO page says reconstruction was then done in Boulder rather than at the telescope, and that reconstructed images are re-ingested for Level 1 conversion to SPEC-0214. Field of view per image is limited by the 4k x 4k detector to 45 x 45 arcsec (blue) or 69 x 69 arcsec (red), covered by tiling ([NSO VBI page](https://nso.atlassian.net/wiki/spaces/PDOI/pages/3534716929)) [V]. The page is an older Confluence version and may lag current practice.
- Beard, Wöger & Ferayorni (2020), "Real-time speckle image processing with the DKIST", SPIE 11452, 114521X: a real-time speckle pipeline for the VBI first-light instrument, C++ implementation, first-light reconstruction shown ([SPIE](https://electronicimaging.spiedigitallibrary.org/conference-proceedings-of-spie/11452/114521X/Real-time-speckle-image-processing-with-the-DKIST/10.1117/12.2563233.full)) [U: only the opening of the abstract was visible via search; the page itself returned empty]. The earlier plan was CUDA acceleration (Wöger & Ferayorni 2012, SPIE 8451, 84511C) [U, snippet].
- The algorithm family is KISIP-style speckle masking in isoplanatic sub-fields (see imaging-methods.md section 6.1). The KIS VTF data-pipeline prototype, delivered to the DKIST Data Center in December 2021, "uses the GPU-integrated speckle code developed by Wöger et al." for broadband channels and Keller & von der Lühe (1992, A&A 261, 321) speckle interferometry for narrowband data ([Leibniz-KIS](https://www.leibniz-kis.de/en/news/article/vtf-data-pipeline-prototype/)) [V]. No algorithm internals, GPU model or benchmark figures are given there.
- Level 1 DKIST FITS conforms to FITS 4 with DKIST header keys; a VBI Level 1 file is a single wideband image with a single timestamp, and each dataset has an ASDF metadata file ([DKIST data centre docs](https://dkistdc-dkist.readthedocs-hosted.com/en/latest/topic_guides/level1data.html)) [U, snippet]. The header dictionary is SPEC-0214 (draft seen: Jan 2019), validated by the `dkist-header-validator` package [U].

I could not find a public DKIST statement that MFBD is part of the VBI production path; the published production path is speckle. The only DKIST MFBD result I found is research (Schirninger et al. 2026, section 2).

### 1.2 SST (SSTRED, MOMFBD)

- SSTRED handles CRISP and CHROMIS. The MOMFBD step was reworked through a fork called REDUX; metadata follow SOLARNET recommendations; arXiv revisions run to v4 (August 2021) and the journal version is A&A 2021 ([arXiv:1804.03030](https://arxiv.org/abs/1804.03030v1)) [V via search summary; journal volume not confirmed].
- Searches for 2023 to 2024 SSTRED or REDUX GPU work found nothing. The SST remains the reference implementation of MOMFBD with sub-fields of about 4 to 5 arcsec (Löfdahl & Hillberg 2022, below) [V].

### 1.3 GREGOR (sTools, HiFI+)

- sTools is IDL-based, Creative Commons licensed, delivers reduced and image-reconstructed HiFI and GFPI data, ranks HiFI frames with MFGS (typically 500 per burst, best 100 restored), and has interfaces to KISIP and MOMFBD ([arXiv:1701.01670](https://arxiv.org/pdf/1701.01670)) [U, snippet].
- HiFI+ (2023, JATIS) offers six bands (Ca II H 396.8 nm, G-band 430.7, blue continuum 450.6, narrow and broad H-alpha 656.3, TiO 705.8 nm); every channel can be restored by speckle masking or MFBD ([IAC listing](https://www.iac.es/es/ciencia-y-tecnologia/publicaciones/improved-high-resolution-fast-imager)) [U, snippet]. I found no sTools 2023 to 2024 changelog.

### 1.4 BBSO GST and NVST

- GST uses post-facto speckle reconstruction for diffraction-limited imaging; deep-learning work around it uses GST as training truth for HMI and GONG super-resolution, not as a replacement for the GST speckle step (A&A 2023 SDGAN, [DeepHMI/SDGAN listing](https://www.aanda.org/10.1051/0004-6361/202349100); [GONG to GST, arXiv:2510.06281](https://arxiv.org/abs/2510.06281v1)) [U, snippets].
- NVST: parallel CPU/MPI speckle (2015 to 2017); the 2017 paper planned a GPU port ([arXiv:1701.00581](https://arxiv.org/pdf/1701.00581)) [U, snippet]. NVST also produced the MFGS metric (section 5).

### 1.5 GPU speckle and GPU MFBD

| Work | What | Source | Tag |
|---|---|---|---|
| Zheng et al. 2018 | CUDA speckle masking, 9 to 10 x speed-up for a 256 x 256 sub-image | [Korea Science](https://koreascience.or.kr/article/JAKO201819757621348.pdf) | [U] |
| Xuan et al. 2020 | CUDA speckle, 2304 x 1984 TiO image in about 70 s, up to 7 x faster than CPU | [J. Atmos. Environ. Opt. 15(2), 90](https://www.researching.cn/articles/OJad771b1dea35f9a8) | [U] |
| Asensio Ramos, Díaz Baso, Kuckein, Esteban Pozuelo, Löfdahl 2025 | torchmfbd: MOMFBD in PyTorch, MAP estimate, spatially variant PSFs, wavefront or data-driven PSF bases (experimental NMF), smoothness and sparsity regularisation, patch processing, apodisation and destretching tools, GPU acceleration, A&A 703, A269, MIT licence, `pip install torchmfbd` | [arXiv:2505.10639](https://arxiv.org/abs/2505.10639), [repo](https://github.com/aasensio/torchmfbd) | [V] |
| Asensio Ramos 2026 | Marginal MOMFBD estimator: adds a log-determinant term to the merit function; regularises, keeps noise out of high-order aberrations, makes object-PSD hyperparameters auto-tunable; integrated into torchmfbd | [arXiv:2605.11980](https://arxiv.org/abs/2605.11980) | [V] |
| Löfdahl & Hillberg 2022 | "Statistical diversity": add random Kolmogorov-statistics high-order aberration tails (scaled by r0) to MFBD; improves contrast and power-spectrum accuracy on noise-free synthetics, same run time; real-data tests still needed. A&A 668, A129 | [arXiv:2205.13650](https://arxiv.org/abs/2205.13650) | [V] |

No timings are published in the torchmfbd abstract (it says only "significantly reduces computation time") [V]. For Helia this means torchmfbd is the practical **reference oracle** for any Rust MFBD: run it on the same synthetic bursts and compare recovered Zernike coefficients, rather than trusting a new implementation unaided.

---

## 2. Machine learning for solar restoration, 2023 to 2026

| Work | Idea | Training signal | Hallucination risk (my assessment) | Optional step? |
|---|---|---|---|---|
| Asensio Ramos, Esteban Pozuelo, Kuckein (2023), "Accelerating Multiframe Blind Deconvolution via Deep Learning", Sol. Phys. ([arXiv:2306.12078](https://arxiv.org/abs/2306.12078)) [V] | A CNN and an **algorithm-unrolled** model (fixed few gradient-descent steps, small networks correcting each step) that output wavefront coefficients and a restored image; orders of magnitude faster than MOMFBD per patch | **Unsupervised**: loss is the image-formation (data-consistency) model on observed CRISP, CHROMIS and HiFI data; trained models, code and data released | Lowest of the set: the forward model is in the loss, so output must re-blur to the input. Residual risk: wavefront prior and generalisation to unseen seeing | Yes, later, as a PSF-estimator or initialiser feeding a classical non-blind step, with the residual reported |
| Asensio Ramos (2024), spatially variant convolution emulator, A&A 688, A88 ([arXiv:2405.09864](https://arxiv.org/abs/2405.09864)) [V] | NN emulates space-variant blur as the MFBD forward model, avoiding patch mosaicking | Synthetic convolutions | Low (forward model only) but PSF-field accuracy unproven on my reading | Research watch |
| Schirninger, Jarolim, Veronig, Kuckein (2025), "Deep learning image burst stacking...", A&A ([arXiv:2506.04781](https://arxiv.org/abs/2506.04781)) [V] | Network turns a 100-frame burst into one image in real time (about 0.5 s per burst [U, snippet]) | **Unpaired image-to-image translation**, speckle reconstructions as the "good" domain | **Highest of the learned-restoration set**: generative translation, "better perceptual quality than speckle" is a perceptual claim, not fidelity. The abstract does not discuss hallucination, names no telescope and links no code [V]. Note: arXiv page lists "A&A 693, January 2025" yet a June 2025 submission; check the bibliographic record | No for default; at most a labelled comparison output |
| Schirninger, Jarolim, Veronig, Rempel, Wöger (2026), "Neural blind deconvolution...", ([arXiv:2603.05033](https://arxiv.org/abs/2603.05033)) [V] | Coordinate network (implicit neural representation) jointly fits object and PSF parameters per burst; tested on MURaM synthetics, GREGOR and DKIST bursts; spatially varying PSFs left as future work | Per-scene physics-informed optimisation, no pretraining | Medium-low in principle (data-consistent by construction) but the abstract has no hallucination discussion and no quantitative comparison; code not linked on the abstract page. A related repository `Schirni/neural-MFBD` (fork of `aasensio/neural-MFBD`) exists with unstated licence ([repo](https://github.com/Schirni/neural-MFBD)) [V] | Watch; the closest thing to "neural fields for seeing" |
| Spagnoletti et al. (2024), diffusion posterior sampling for astronomical deconvolution ([arXiv:2411.19158](https://arxiv.org/abs/2411.19158)) [V] | Score-based prior trained on cosmological simulations plus likelihood; proposes a metric flagging prior-driven features | Simulations | High by nature (strong learned prior), though this paper is the only one found that explicitly **quantifies** prior-driven features. Not solar | No |
| Space-to-ground super-resolution (diffusion, GAN, Real-ESRGAN) of HMI or GONG using GST as truth, 2022 to 2025 [U, snippets] | Enhance low-resolution full-disc products | GST images | High; these invent detail to a learned target and are not restoration of a burst | No, out of scope for Helia input |
| General turbulence-mitigation video networks (TMT, Tran. Comput. Imaging 2024; DATUM, CVPR 2024; DeTurb, ACCV 2024) [U, snippets] | Terrestrial scenes with simulated turbulence | Synthetic | Unknown on solar granulation; trained on a different statistics regime | No |
| Jarolim et al. (2020), full-disc image quality assessment with GANs, A&A ([arXiv:2008.12030](https://arxiv.org/pdf/2008.12030)) [U, snippet] | Unsupervised whole-disc quality classifier for Kanzelhöhe H-alpha | Good-quality frames only | Low (only ranks) | Relevant to #8 as a frame-level scorer |

Honest assessment:

1. The two papers that keep the physical forward model inside the training or inference loop (Asensio Ramos et al. 2023, Schirninger et al. 2026) are the only ones I would consider defensible in a scientific tool. Everything trained on "speckle result as target" or on simulations with a hard prior inherits that target's biases, which was already the finding for 2018 networks (imaging-methods.md section 9.1).
2. No paper I found tests for hallucination with **injected features absent from the input**, which is the test Helia's synthetic harness can do. That is both a gap and an opportunity: Helia can publish the test.
3. Suitability as optional, clearly labelled steps: acceptable if (a) the original stack is always written first, (b) the ML output is a separate layer with a visible badge and FITS provenance (`PRSTEPn='ML-...'`, see section 6), (c) a data-consistency residual and an input-shuffle consistency map accompany it, (d) the model is a plug-in outside the core pipeline. Frame-quality scoring (#8) is the safest ML use because it never writes pixels.

---

## 3. Amateur and open tools, state in October 2026

| Tool | Version and date | Licence, platform | What matters for Helia | Source |
|---|---|---|---|---|
| **AutoStakkert!4** | 4.0.1 beta 2023-11-11 to **4.0.13 2025-02-19** (4.0.11, 2024-01-21 is "reasonably stable"). Adds data browser, **automated surface anchor placement**, **automated quality estimation settings**, improved surface tracking for large jumps ("Improved Tracking"), H-alpha AP placement fix for colour cameras, AVX2 line-artefact fix, RGB align/derotation fixes. Change notes never mention "solar", flats or GPU | Licence terms: see autostakkert.com. Built with Delphi for Windows (change notes) [V] | Auto anchor and auto quality settings are the AS!4 differentiators Helia lacks. No algorithm description was found | [download](https://www.autostakkert.com/wp/download/), [change notes](https://www.autostakkert.com/wp/change-notes/) [V] |
| **Siril** | **1.4.4 stable, 2026-06-17**; 1.4.0 stable 2025-12-05. No 1.5 or release candidate listed on siril.org. 1.4 headline features are deep-sky (new drizzle, astrometric mosaics, SIP distortion, FITS keyword management, SPCC) | GPLv3 (repository LICENSE.md, checked October 2026); Windows, Linux, macOS (siril.org) [V] | **Multi-point registration and stacking "was introduced in Siril 1.5"** (documentation) and is **not in any released Siril**. Docs describe: Planet (centroid) and Surface (detail) modes, reference = best 5 per cent, grid pitch 2.25 x half-width, search 14 px, global search 34, refine-against-first-pass default on, shift smoothing radius 2.5, zero-mean option, Min brightness/contrast/structure AP rejection, 1x/1.5x/2x/3x scale, patch engine default and experimental warp engine, default demosaic LMMSE, `mpp` commands. Reworked with input from PSS's author | [siril.org](https://siril.org/), [downloads](https://siril.org/download/), [1.4.0](https://siril.org/download/2025-12-05-siril-1-4-0/), [multipoint docs](https://siril.readthedocs.io/en/latest/preprocessing/multipoint.html) [V] |
| **PlanetarySystemStacker** | Release list: V0.5.0 (first) to **V0.9.8** (Windows installer, fixes PyInstaller problem); V0.8.31 added stack-N-or-percent; V0.8.18 added RGB-align postprocessing. The release page shows days and months only, no years. Debian/Ubuntu still package a 2023-09 snapshot (0.8.32) | GPLv3 (header, from existing review), Python | Nothing solar-specific added; maintained at a slow cadence | [releases](https://github.com/Rolf-Hempel/PlanetarySystemStacker/releases) [V for the list, years unknown] |
| **ImPPG** | **2.1.0**, "18 Nov" (year not shown; existing review says 2025) | GPLv3, C++/OpenGL | Scripting (Lua), multiple unsharp masks, RGB, adaptive unsharp, solar limb alignment, dering; still the live-preview benchmark | [releases](https://github.com/GreatAttractor/imppg/releases) [V; year U] |
| **RegiStax** | 6.1.0.8 (May 2011) is final; page states "This marks the end of development for RegiStax" | Site header: "Free image processing software"; downloads are Windows .exe files [V] | Development ended. Its wavelet sharpening lives on in **waveSharp** (1.0 beta Dec 2023, 2.0 Dec 2024, **3.0 2025-12-09**, hosted on Codeberg) and AS!4 now "sends to waveSharp" | [astronomie.be](https://www.astronomie.be/registax) [V] |
| **LuckyStackWorker** | 5.2.0 (2024-06-23); a search report says 7.0.0 in Jan 2026 | Licence and platforms per the project page, not re-checked [U]; author Wilco Kasteleijn | Post-stack automation: per-object profiles recognised from FireCapture/AS file names, bulk processing, adaptive sharpening and denoising (Savitzky-Golay 2D, sigma). A workflow model, not an algorithm one | [ScopeTrader](https://scopetrader.com/luckystackworker-automates-solar-image-processing/) [V for 5.2.0]; 7.0.0 [U] |
| **SolHAT** (successor to `solar_ha_processing`) | No releases listed, 39 commits, 2 stars | MIT, **Rust**, CLI; Kevin M. Gill (kmgill) | **Closest prior art to Helia**: H-alpha lucky imaging replacing PIPP and AutoStakkert; flat and dark correction, glitch-frame detection, quality filtering, centre-of-mass centring, crop, **parallactic-angle derotation for alt-az mounts**, drizzle 1.0 to 3.0 x, optional **prominence light set** input, user hot-pixel TOML map; no GPU, no multi-point, no limb-darkening feature listed. The older `solar_ha_processing` repo is declared replaced | [solhat](https://github.com/kmgill/solhat), [old repo](https://github.com/kmgill/solar_ha_processing) [V] |
| **JSol'Ex** (astro4j) | 5.5.2 (date not shown; 5.6.0-SNAPSHOT in development) | Apache-2.0, Java 25, Win/Linux/macOS; Cédric Champeau | For Sol'Ex spectroheliograph SER, not video lucky imaging, but its feature list is the best open model of solar output handling: **limb darkening, colourisation, prominences, mosaic, animation, FITS/SOLARNET metadata, ImageMath, Doppler, autocrop**. A 2025 spectroheliogram atlas and a 2026 G-band prominence paper both used it ([arXiv:2507.13025](https://arxiv.org/pdf/2507.13025), [arXiv:2604.11650](https://arxiv.org/pdf/2604.11650)) [U, snippets] | [astro4j](https://github.com/melix/astro4j), [docs](https://melix.github.io/astro4j/latest/) [V for the feature list]; Sol'Ex only [U] |
| **INTI** | Python Sol'Ex pipeline, V. Desnoux / C. Buil | [U] | Predecessor of JSol'Ex for spectroheliography | [U] |
| **PIPP**, **SER Player** | No successor found. SER Player (free-astro) is a viewer on Flathub | [U] | PIPP's job (crop, centre, reject frames, calibrate) is what Helia's input stage should replace | [U] |
| **AstroSurface** | astrosurface.com (French): "astronomical image and video processing, Windows 64 bits"; "experimental, non-commercial use"; a wavelet-deconvolution tool is mentioned | [V] | Nothing beyond the site is claimed | [astrosurface.com](https://www.astrosurface.com/) [V] |
| **WinJUPOS** | Planetary derotation tools; no solar full-disc workflow found | Windows, or Linux through WineHQ, per its download page (jupos.org) [V]; licence not restated | Solar derotation is not a WinJUPOS strength; AS!4 does RGB-channel field derotation itself | [U] |

Features those tools have that Helia lacks, ranked by impact on a solar user:

1. Flat/dark/hot-pixel calibration (SolHAT, PSS, SharpCap): #3.
2. Automatic anchor/AP placement and auto quality settings (AS!4); AP rejection thresholds (Siril brightness/contrast/structure): #16.
3. Refinement against a first-pass stack, shift smoothing, zero-mean NCC (Siril 1.5 docs): #4.
4. Alt-az field-rotation (parallactic) correction (SolHAT, AS): not in any issue.
5. Prominence light-set compositing (SolHAT input, JSol'Ex): not in any issue.
6. Limb-darkening flattening, colourisation, autocrop, mosaic, animation (JSol'Ex): not in any issue.
7. RL/unsharp/tone-curve with live preview, scripting (ImPPG): #5.
8. SOLARNET/FITS metadata (JSol'Ex, SSTRED, DKIST): not in any issue.
9. Drizzle, warp engine (SolHAT, PSS, AS, Siril): #6.
10. Demosaic variety (Siril default LMMSE; Helia has bilinear and MHC): low priority for mono H-alpha.

---

## 4. Destretching and flow-based registration

- **Destretching** is local correlation against a reference then interpolation (November & Simon 1988); it is the same family as LCT/FLCT and as the AP grid [V via search summary of [arXiv:0707.1994](https://arxiv.org/pdf/0707.1994)]. torchmfbd ships destretching and apodisation tools as first-class utilities [V], which confirms it is still the professional pre-step.
- **Method comparison on solar data**: Chae & Sakurai (2008) tested LCT, DAVE and NAVE on simulated and Hinode data: NAVE best at sub-pixel, super-pixel and non-uniform motions, LCT the fastest and weakest on non-uniform motion, NAVE the slowest ([summarised in search result](https://arxiv.org/pdf/0707.1994)) [U, secondary]. This concerns velocity measurement, not seeing correction. LCT/feature tracking was found unreliable below about 2500 km and 30 min by Rieutord et al. (2001) [U, secondary]. A 2025 preprint applies Fourier LCT to MURaM ([arXiv:2511.19048](https://arxiv.org/pdf/2511.19048v1)) [U].
- **Turbulence-distortion registration outside solar**: Zhu & Milanfar (B-spline non-rigid registration to a temporal-average reference, temporal regression, then blind deconvolution; [UCSC ICCP 2011](https://users.soe.ucsc.edu/%7emilanfar/publications/conf/ICCP11.pdf)) [U, snippet]. Shimizu et al. (CVPR 2008) added a stabilisation term for textureless regions, which Zhu & Milanfar criticised as bias [U, snippet]. A 2026 Gaussian-splatting turbulence mitigation paper (GSTurb, [arXiv:2602.22800](https://arxiv.org/pdf/2602.22800)) again uses a multi-frame average as the tilt-free reference [U, snippet].
- **TV-L1, learned flow**: I found no 2022 to 2026 solar comparison. TV-L1 is available as a GPU algorithm in OpenCV [U, background]. Granulation is low contrast and evolves, which violates brightness constancy in a way AP correlation (with a small window and a short time span) tolerates better. Learned flow (RAFT-style) would be a trained black box on a task with no ground truth.
- **Comparison with AP grids**: the practical difference is smoothness. Siril's experimental **warp engine** already interpolates AP shifts into one smooth field and resamples each frame once, and its docs say it "may be slightly crisper" but slower [V]. That is the pragmatic version of dense flow: use the AP grid as the sparse estimator and make the dense field by robust smoothing, rather than running a separate flow solver per frame.

Recommendation: keep AP-grid correlation as the estimator; add robust shift-field smoothing and a single-resample warp output (later). Run TV-L1 only as an experiment inside the synthetic harness, scoring recovered shift field RMS against the known truth tilt field; no current evidence supports spending effort before that result exists.

---

## 5. Frame and patch quality metrics

**Correction to the existing review: comparison studies do exist.**

- **Popowicz, Radlak, Bernacki, Orlov (2017)**, "Review of image quality measures for solar imaging" ([arXiv:1709.09458](https://arxiv.org/abs/1709.09458)) [V abstract]: reviews 36 methods from 40 years; ground truth is Hinode/SOT images degraded with a Random Wave Vector turbulence simulation (active regions and granulation, many D/r0 values). Three recommended: **Helmli and Scherer's Mean** (lowest computational cost, usable in virtually any conditions), **MFGS** (best at good seeing, D/r0 below 4), **DCT Energy Ratio** (best for strongly blurred images). This is the closest thing to the head-to-head truth comparison the old review said did not exist, and it uses simulated seeing with known truth.
- **Kamlah, Verma, Diercke, Denker (2021)**, "Wavelength dependence of image quality metrics and seeing parameters and their relation to adaptive optics performance", Solar Physics ([arXiv:2011.04346](https://arxiv.org/abs/2011.04346)) [V abstract]: MFGS, Median Filter Laplacian Similarity (MFLS), Helmli-Scherer mean, granular rms contrast and differential image motion compared against r0 on HiFI/VTT quiet-Sun data (July and August 2016); UMAP used to characterise seeing; per-wavelength dependence and AO field dependence. No numerical ranking in the abstract.
- **Deng et al. (2015)**, MFGS definition: a 3 x 3 median-filtered copy of the frame, gradient images of both, a similarity ratio giving a score between 0 and 1 that falls monotonically with quality and correlates linearly with granular rms contrast; tested on NVST and Hinode/SOT ([NJIT record](https://digitalcommons.njit.edu/fac_pubs/7006), [arXiv:1701.05300](https://arxiv.org/abs/1701.05300)) [V]. Denker et al. (2018) moved to a Scharr-operator gradient magnitude [U, snippet].
- No 2023 to 2026 solar frame-metric comparison found. The ML route to quality is Jarolim et al. (2020) whole-disc classifier and Gao et al. (Sensors, Dec 2023) PSF-to-Airy-similarity network for night-time extended-target AO frame selection ([PMC10781174](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC10781174/)) [U, snippets].

What this changes for Helia: the old review's hypothesis (band-limited Fourier power with noise floor) is partly supported (DCT energy ratio is a transform-domain energy measure that wins in poor seeing). Candidate set for the harness: gradient energy, Laplacian std (built), **Helmli-Scherer mean**, **MFGS**, **DCT-ER**, band-limited Fourier power with noise floor. Expect different winners by D/r0, so the metric (or a blend) should depend on estimated seeing. All of Popowicz's findings are on simulated Hinode truth with a particular turbulence generator [V], so re-check on Helia's own harness before fixing a default.

MFGS successors: MFLS (Kamlah et al.) and the Scharr-gradient variant are the only ones found [V/U as above].

---

## 6. Full-disc processing and metadata

### 6.1 Limb darkening

- **Empirical radial profile** is the established route for H-alpha and Ca K full-disc images: convert to polar coordinates, take the **median in the azimuthal direction** (median rather than mean because plages and filaments are high contrast), and divide by the reconstructed 2D profile ([Kodaikanal H-alpha, arXiv:1608.03390](https://arxiv.org/pdf/1608.03390); [arXiv:1110.3894](https://arxiv.org/pdf/1110.3894) citing Denker et al. 1999) [U, snippets]. A polynomial fit to the average centre-to-limb variation (5th order) is the alternative ([arXiv:0905.3055](https://arxiv.org/pdf/0905.3055)) [U].
- **Filter transmission is a separate term**: for Lyot-filter He I (ChroTel) a symmetric limb-darkening function alone leaves filter-transmission artefacts, fitted with Zernike polynomials ([arXiv:1812.04404](https://arxiv.org/pdf/1812.04404)) [U]. This maps onto the etalon "sweet spot" in the old review: model the instrument gradient and the physical CLV separately when possible.
- **Per-site signature**: NSO/NISP H-alpha Carrington maps build radial mean profiles per GONG site and model them with Legendre polynomials because each site has a unique limb-darkening signature ([arXiv:2608.13812](https://arxiv.org/pdf/2608.13812), an unusually recent ID, verify) [U].
- **White light**: Neckel & Labs (1994) fifth-order polynomials in mu at 30 continuum wavelengths (303 to 1099 nm), data within 7 arcsec of the limb excluded (mu about 0.12); later work gives about 0.4 per cent accuracy and possible scattered-light over-correction ([e.g. arXiv:2310.05652](https://arxiv.org/pdf/2310.05652)) [U, secondary]. A related Neckel (2003/2005) Solar Physics series uses the same coefficients [U].

### 6.2 Compositing and colour

- **Prominence plus disc**: SolHAT takes an optional prominence light set; amateur practice is separate exposures blended through a limb mask (existing review 8.2). No standard exists beyond practice [U]. Recommend linear-float compositing with a feathered limb mask, single tone map afterward.
- **Colour conventions**: no formal convention found. Amateur practice is greyscale, pseudo-colour, partly inverted and inverted pseudo-colour variants per filter; H-alpha is commonly rendered red-orange; no source for a Ca K false colour ([Stargazers Lounge 2023](https://stargazerslounge.com/topic/410711-june-11-2023-sun-in-three-wavelengths), [S&T gallery](https://skyandtelescope.org/online-gallery/sun-cak/)) [U]. Keep the data mono float and apply colour as a declared LUT at export (as the old review said); JSol'Ex does "colourisation" as a named feature [V].

### 6.3 Mosaicking

Amateur full-disc H-alpha mosaics are typically 13 to 20 panes: AS!3 per pane, ImPPG sharpen, then **MS-ICE**, Autopano Giga, Affinity Photo or GIMP ([example posts](https://stargazerslounge.com/topic/398046-august-8-2022-a-13-pane-full-disk-h-alpha-mosaic)) [U, snippets]. I could not confirm that Microsoft ICE is discontinued. Siril 1.4 has astrometric alignment for mosaics with little overlap and WCS-SIP distortion correction, but for deep-sky plate solving ([siril.org 1.4.0](https://siril.org/download/2025-12-05-siril-1-4-0/)) [V]. JSol'Ex lists "mosaic" as a feature [V].

### 6.4 Derotation

- Two different operations must not be conflated: **field rotation of alt-az mounts** (parallactic angle from time and site; SolHAT does it, AS offers alt-az derotation [V: AS 2013 beta note via the AS blog; SolHAT README]) and **solar rotation** (differential, needs P, B0, L0). The old review covers only the second.
- Differential rotation coefficients: the form 14.713 - 2.396 sin^2(phi) - 1.787 sin^4(phi) degrees per day is cited as the accepted average set with signs negative on B and C ([EPFL concept page](https://graphsearch.epfl.ch/en/concept/1359541)) [U]; another paper quotes the Snodgrass & Ulrich (1990) magnitudes as 14.713, 2.293, 1.787 ([arXiv:2204.09014](https://arxiv.org/pdf/2204.09014)) [U]. The middle coefficient is therefore **disputed in my sources (2.396 or 2.293)**; check the paper before coding.
- WinJUPOS derotation workflows found are planetary (Jupiter/Mars). No solar full-disc workflow found [U].

### 6.5 Metadata: SOLARNET recommendations

SOLARNET Metadata Recommendations for Solar Observations, Haugan & Fredvik, arXiv:2011.12139, **v4 revised 2024-03-18** (document header "Version 2.2 - 18. March 2024"; the Read the Docs site additionally lists releases 3.0.0 on 2025-02-04, 3.1.0 on 2025-06-25 and 3.2.3 undated) ([arXiv](https://arxiv.org/abs/2011.12139), [docs](https://solarnet-metadata.readthedocs.io/en/stable/)) [V]. Points read from the text:

- Every fully or partially compliant observation HDU must carry **EXTNAME, SOLARNET, OBS_HDU, DATE-BEG** (SOLARNET = 1 fully, 0.5 partially; OBS_HDU = 1) [V].
- WCS: use **HPLN-TAN and HPLT-TAN** for solar coordinates (Thompson 2006), plus WAVE, UTC, STOKES where relevant. For plate scale derived from the apparent solar radius, report **RSUN_REF** (Thompson 2010 section 8) [V].
- **Ground-based observatories must report OBSGEO-X, -Y, -Z** (GPS is an acceptable proxy for ITRF). DSUN_OBS, HGLN_OBS, HGLT_OBS are required for deep-space missions and optional for others [V].
- Time: DATE-BEG mandatory, DATE-AVG optional; exposure in **XPOSURE** not EXPTIME; **NSUMEXP** and **TEXPOSUR** for summed frames [V].
- Provenance: **PRSTEPn** (comma-separated list for inseparable steps, in order, e.g. FLATFIELDING, CALIBRATION, DISTORTION-CORRECTION) with optional **PRPROCn, PRPVERn, PRMODEn, PRPARAn, PRREFn, PRLOGn, PRENVn** and library keywords PRLIBna/PRVERna; the worked example uses `PRSTEP1='MOMFBD'` [V].
- Also CAMERA, DETECTOR, FILTER, XPOSURE etc. as instrument keywords [V].

For Helia the mapping is direct: SER trailer timestamps give DATE-BEG/DATE-AVG/DATE-END; frame count and selection fraction give NSUMEXP; each pipeline stage (calibration, AP alignment, weighted stack, wavelet or RL sharpening, any ML step) is one PRSTEPn. Helioprojective WCS needs a limb fit (centre, radius in pixels) plus an assumed or ephemeris solar angular radius to set CDELT in arcsec; mark the WCS "approximate" unless an ephemeris solar radius and observer position are used. Computing the ephemeris itself is not required to write valid headers.

---

## 7. Calibration best practice for solar narrowband

- **Flats from displaced Sun images** (Kuhn, Lin & Loranz 1991, PASP 103, 1097): record many images of the disc at different pointings so the disc falls on different pixels; solve gain tables iteratively; the Rome PSPT group reports relative-gain accuracy better than 1e-4 ([OA Roma](https://www.oa-roma.inaf.it/data-calibration/), [Kanzelhöhe, arXiv:2111.03176](https://arxiv.org/pdf/2111.03176)) [U, snippets]. Toussaint, Harvey & Toussaint improved convergence with over-relaxation and fractional-pixel shifts; a 2020 Solar Physics KLL variant with small offsets and stitching reports better than 0.1 rms across the field ([Springer](https://link.springer.com/content/pdf/10.1007/s11207-020-1583-7.pdf)) [U]. A 2025 paper extracts the flat from real-time observation data by correcting an average flat with KLL, because pointing and **column fixed-pattern noise** of CMOS detectors change during observing and leave residuals ([arXiv:2503.15094](https://arxiv.org/abs/2503.15094)) [U, snippet].
- **Column FPN**: a two-way (vertical guided) filter estimates and removes column fixed-pattern noise from solar images ([NJIT record](https://digitalcommons.njit.edu/fac_pubs/143)) [U, snippet; year not confirmed].
- **Gain table model**: corrected = gain x (raw + offset), gain and offset row, column or pixel dependent (camera vendor FFC documentation) [U]; sCMOS pixel-gain noise is multiplicative and scales with illumination [U, one microscopy source]. Hence a dark/bias frame matched to gain and exposure, plus a flat taken at the science focus.
- **Etalon fringes and Newton rings**: shift with focus and tilt, so flats must be taken at science focus (existing review 2.3, still unverified beyond amateur sources); the displaced-Sun method naturally reproduces the fringe pattern because it is taken through the same optical path [U inference, mine].
- I found **no** published per-pixel gain-table study specific to a high-frame-rate amateur or professional sCMOS solar camera, nor a dark-frame versus flat-table comparison for it.

Design implication: #3 should offer (a) master dark, (b) master flat by median of defocused or displaced-disc frames with the disc masked, (c) hot-pixel map, (d) column-FPN removal option, (e) later, a KLL-style gain-table solve from a displaced-disc SER.

---

## 8. Method matrix

Columns: maturity P production / R research / E experimental. Compute cost: low / med / high (per full capture on a consumer GPU). GPU fit: good / fair / poor. In Helia: built / planned #N / missing. Recommendation: MVP+ / next / later / skip.

| Method | Category | Source | Maturity | Compute | GPU fit | In Helia now? | Recommendation and reason |
|---|---|---|---|---|---|---|---|
| Dark, flat, hot-pixel calibration | Calibration | SolHAT, PSS, existing review 2.3 [V] | P | low | good | planned #3 | MVP+: fixed dust and fringes are sharpened by the warp otherwise |
| Column FPN removal | Calibration | [NJIT](https://digitalcommons.njit.edu/fac_pubs/143) [U] | P | low | good | missing | next: cheap, CMOS-specific |
| Displaced-Sun gain table (KLL) | Calibration | Kuhn et al. 1991 [U] | P | med | good | missing | later: best flat, needs a special capture |
| Limb-fit / centroid global alignment | Registration | Siril Planet mode, SolHAT, ImPPG [V] | P | low | good | missing | MVP+: full-disc frames lack a patch to correlate |
| Phase-correlation global shift | Registration | existing review 4.2 | P | low | good | built | keep; return a confidence value |
| AP-grid multi-point NCC | Registration | PSS, Siril docs [V] | P | med | good | built | keep |
| Two-pass AP refinement vs local stack | Registration | Siril 1.5 docs: default on, 5 per cent of frames clamped 8 to 32 [V] | P | med | good | planned #4 | MVP+: removes reference bias |
| Robust shift-field smoothing | Registration | Siril docs, radius 2.5 grid steps [V] | P | low | good | missing (part of #4) | MVP+: cheap, cuts measurement noise |
| Zero-mean NCC option | Registration | Siril docs [V] (about doubles failures on low-contrast boxes) | P | low | good | missing | next: option, default off |
| AP rejection (brightness, contrast, structure, limb mask, timestamp burn-in) | Registration | Siril docs, #16 [V] | P | low | good | planned #16 | MVP+: avoids bad AP shifts at the limb |
| Automatic anchor and quality-parameter selection | Usability | AS!4 4.0.1 [V] | P | low | good | missing | next: key AS!4 convenience |
| Gradient and Laplacian frame/AP quality | Quality | PSS, Siril [V] | P | low | good | built | keep |
| Helmli-Scherer mean, MFGS, DCT energy ratio | Quality | Popowicz et al. 2017 [V] | P | low | good | missing | next: choose by D/r0 in the harness |
| Noise-floor band-limited Fourier power | Quality | imaging-methods.md 3.2 (hypothesis) | R | med | good (FFT) | missing | next: test in the harness; evaluate `wgsl-fft` |
| Learned frame-quality scorer | Quality/ML | Jarolim 2020 [U], Gao 2023 [U] | R | low | good | planned #8 | later: safe use of ML, no pixel output |
| Quality-weighted mean stack, soft cutoff | Stacking | Siril, PSS [V] | P | low | good | built (weighted mean) | keep |
| Sigma clip | Stacking | existing review 5.1 | P | low | good | built | keep, off by default |
| Drizzle / splat accumulator | Stacking | Fruchter & Hook 2002; SolHAT 1x to 3x; Siril 1x to 3x [V] | P | med | good | planned #6 | later: only when sampling q < 1; design as splat now |
| Dense warp, single resample | Stacking | Siril warp engine (experimental) [V] | E | med | good | missing | later: removes patch seams |
| TV-L1 or learned optical flow | Registration | none solar found | E | high | fair | missing | skip: test in harness first |
| B-spline non-rigid registration | Registration | Zhu & Milanfar [U] | R | high | poor | missing | skip |
| Alt-az field rotation (parallactic) | Geometry | SolHAT, AS [V] | P | low | good | missing | next: needs SER timestamps and site |
| Solar differential derotation | Geometry | existing review 8.5; coefficients disputed [U] | P | low | good | missing | later: only for long captures and mosaics |
| Unsharp mask, tone curve | Sharpening | ImPPG [V] | P | low | good | planned #5 | MVP+ |
| À trous wavelets with noise threshold | Sharpening | Starck et al.; waveSharp 3.0 [V for waveSharp] | P | low | good | planned #5 | MVP+ |
| Non-blind Richardson-Lucy, Gaussian + Airy | Sharpening | ImPPG 2.1.0 [V] | P | med | good | planned #5 | MVP+: live preview |
| TV or damped RL, Wiener | Sharpening | Dey et al. 2006 (existing) | P | med | good | missing | later |
| Limb-aware masked deconvolution | Sharpening | existing review 7.4 | P | low | good | missing | MVP+ with RL: avoids limb ringing |
| Radial limb-darkening flattening (azimuthal median) | Full disc | Kodaikanal H-alpha [U] | P | low | good | missing | MVP+: needed by sharpening and compositing |
| Neckel & Labs CLV polynomials | Full disc | [U] | P | low | good | missing | later: white light only |
| Prominence and disc HDR compositing | Full disc | SolHAT, JSol'Ex [V] | P | low | good | missing | next |
| Colourisation LUT at export | Full disc | JSol'Ex [V] | P | low | good | missing | next |
| Mosaic panel registration and blend | Full disc | amateur practice [U] | P | med | fair | missing | later |
| SOLARNET FITS header, PRSTEPn provenance, HPLN-TAN WCS | Metadata | Haugan & Fredvik, v2.2 [V] | P | low | n/a | missing | MVP+: cheap, costly to retrofit |
| Time-lapse and animation | Output | JSol'Ex [V] | P | low | good | missing | later |
| Per-sub-field MFBD, Zernike PSFs | Restoration | torchmfbd [V], Löfdahl & Hillberg 2022 [V] | R | high | fair (small FFTs) | planned #7 | later: validate vs torchmfbd; include statistical high-order tails and marginal term |
| Speckle masking, Knox-Thompson | Restoration | KISIP, DKIST VBI [V/U] | P (professional) | high | fair | missing | skip: dominated by MFBD for Helia's inputs |
| Phase diversity | Restoration | existing review | P (professional) | high | fair | missing | skip: needs a second channel |
| Unsupervised/unrolled MFBD network | ML | Asensio Ramos et al. 2023 [V] | E | low at inference | good | missing | later: optional PSF estimator, labelled |
| Burst-to-image translation network | ML | Schirninger et al. 2025 [V] | E | low | good | missing | skip: perceptual training target |
| Neural-field blind deconvolution | ML | Schirninger et al. 2026 [V] | E | high (per scene) | fair | missing | skip now; watch |
| Diffusion posterior sampling | ML | Spagnoletti 2024 [V] | E | high | fair | missing | skip: strong prior, not solar |
| Fourier ring correlation for resolution | Diagnostics | existing review 12.4 [U] | P | low | good | missing | next: harness metric |
| GPU FFT | Infrastructure | `wgsl-fft` 0.5.1, MIT, Stockham radix-4/2 plus Bluestein, batched, released 2026-09-03; 2D and f32 precision not stated [V] | E | n/a | good | missing | next: spike before writing own FFT |

---

## 9. Proposed milestones

M1 is the built MVP (SER/AVI/FITS input, bilinear and MHC debayer, frame and per-AP quality, phase correlation, AP grid, weighted mean, optional sigma clip).

**M2: trustworthy stacking inputs and outputs.** #3 calibration (dark, flat at science focus, hot pixels, column FPN option); #16 AP masking (limb mask, burn-in timestamp, brightness/contrast/structure rejection); #4 two-pass refinement plus shift-field smoothing; limb-fit global alignment with confidence; FITS output with SOLARNET minimum (EXTNAME, SOLARNET, OBS_HDU, DATE-BEG, XPOSURE, NSUMEXP, OBSGEO-X/Y/Z, PRSTEPn). Why first: every later step is judged on this base; flats and AP masking fix the largest known solar failure modes; the metadata is cheap now and painful to retrofit.

**M3: look and measure.** #5 sharpening (unsharp, à trous with noise threshold, non-blind RL with Gaussian + Airy and a limb-aware mask, live preview); radial limb-darkening flattening; synthetic-harness metric bake-off (gradient, Laplacian, Helmli-Scherer, MFGS, DCT-ER, Fourier band power) and quality-versus-rank diagnostics; FRC; spike `wgsl-fft`. Why: gives users a complete workflow and gives designers the evidence for the quality default.

**M4: solar presentation.** Prominence/disc HDR compositing, colourisation LUT export, helioprojective WCS from the limb fit, alt-az field-rotation correction using SER timestamps, autocrop, auto anchor/AP parameters, displaced-Sun gain-table flats. Why: these are the features JSol'Ex and SolHAT users already have and are missing from every issue.

**M5: geometry and sampling.** #6 drizzle on a splat accumulator (enabled when q < 1), dense warp single-resample engine, panel mosaicking, solar derotation, time-lapse. Why: need M2 diagnostics to show benefit and M4 geometry to be correct.

**M6: research track.** #7 per-sub-field MFBD validated against torchmfbd on harness bursts, with Löfdahl & Hillberg statistical tails and the marginal term; #8 learned quality scorer; optional unsupervised PSF-estimator plug-in; all labelled experimental. Why: highest cost, least certain gain for amateur apertures; the harness and FFT groundwork come from M3.

---

## 10. "Keep it current" checklist (re-check at each release)

1. Siril: siril.org/download for a 1.5 release or candidate, and the multipoint documentation page (the feature is documented but unreleased at the time of writing).
2. AutoStakkert!: autostakkert.com download page and change-notes page for 4.0.x or 5.x, especially solar, flats or GPU entries.
3. PlanetarySystemStacker, ImPPG, waveSharp (Codeberg), LuckyStackWorker, JSol'Ex/astro4j, SolHAT: GitHub/Codeberg releases pages (years are not shown on some; check the tags).
4. torchmfbd repo and A&A listing; arXiv astro-ph.IM and astro-ph.SR for "multi-frame blind deconvolution", "solar speckle", "burst" (search each of Asensio Ramos, Löfdahl, Schirninger, Wöger, Jarolim as authors).
5. Metrics: Solar Physics and A&A for "image quality" with Denker, Popowicz, Kamlah, Deng as authors; arXiv 1709.09458 follow-ups.
6. Professional pipelines: NSO DKIST data-centre docs (VBI page, SPEC-0214, `dkist-header-validator`), KIS VTF news, AIP GREGOR/sTools pages, SSTRED/REDUX repository.
7. Metadata: SOLARNET Read the Docs release list and arXiv:2011.12139 version; FITS standard releases; astropy/sunpy WCS changes affecting HPLN-TAN.
8. Infrastructure: crates.io pages for `wgsl-fft`, `gpu-fft`, `wgpu`, `ort`, `burn`, `tract`.
9. ML: arXiv for "hallucination" or "uncertainty" in solar restoration; any paper using injected-feature tests.
10. Conferences: SPIE Astronomical Telescopes + Instrumentation, SPD/AAS, SOLARNET and DKIST data-user workshops.

---

## 11. Corrections to imaging-methods.md

1. **Section 3.2 and preamble: "no peer-reviewed head-to-head comparison of frame-quality metrics on solar data" is wrong.** Popowicz et al. (2017, arXiv:1709.09458) compared 36 methods against simulated-truth Hinode images and recommend Helmli-Scherer mean, MFGS (D/r0 < 4) and DCT energy ratio; Kamlah et al. (2021, arXiv:2011.04346) related MFGS, MFLS, Helmli-Scherer, rms contrast and differential image motion to r0 on HiFI data [V].
2. **Sections 4.3 and 10: Siril multipoint is listed as a Siril feature.** The documentation states it "was introduced in Siril 1.5". siril.org lists 1.4.4 (2026-06-17) as latest stable and no 1.5 build or RC. The row should read "Siril 1.5 (documented, unreleased)". The detailed defaults the old review quoted (2.25 x half-width, search 14, smoothing 2.5, best 5 per cent reference, scales 1/1.5/2/3, patch and experimental warp engines) all match the current docs [V]. New detail: default demosaic LMMSE (earlier default RCD), `mpp`/`register_mpp`/`stack_mpp` commands.
3. **Section 3.1, MFGS: "original definition and authorship not verified".** MFGS is Deng et al. (Solar Physics 2015; arXiv:1701.05300, authors Deng, Zhang, Wang, Ji, Wang, Liu, Xiang, Jin, Cao): 3 x 3 median filter, gradient comparison, 0 to 1 score, linear with granular rms contrast, tested on NVST and Hinode/SOT [V]. The old table row's "gradient similarity" is right; later variants use a Scharr gradient magnitude [U].
4. **Sections 4.3, 10: AutoStakkert!3 only.** AutoStakkert!4 exists: 4.0.1 (2023-11-11) to 4.0.13 (2025-02-19), beta status for 4.0.13, with automated surface-anchor placement and automated quality estimation [V]. The licence and price are not stated on the pages read and are no longer claimed; the change notes describe Delphi-built Windows software [V].
5. **Section 10: Registax.** Development ended: final version 6.1.0.8 (May 2011), stated on the official page. Wavelet sharpening continues in waveSharp (3.0 on 2025-12-09) [V].
6. **Section 10: missing tools.** Add SolHAT (Rust, MIT, closest prior art), JSol'Ex (Apache-2.0, solar output features), LuckyStackWorker (MIT), waveSharp [V]. The claim that "only ImPPG uses a GPU" remains true for the tools checked; SolHAT lists no GPU use [V].
7. **Section 6.2, torchmfbd: bibliography.** It is Asensio Ramos, Díaz Baso, Kuckein, Esteban Pozuelo & Löfdahl (2025), A&A 703, A269, MIT licence; the old text says only "arXiv:2505.10639". The "Marginal MFBD" reference is Asensio Ramos (2026), single author, submitted 2026-05-12 [V]. The "MFBD with uncorrected high-order modes (2022, authors not verified)" reference is Löfdahl & Hillberg (2022), A&A 668, A129 [V].
8. **Section 6.2 and 11.2, wgpu FFT: "did not find a mature wgpu FFT crate".** `wgsl-fft` 0.5.1 (MIT, Stockham radix-4/2, Bluestein, batched, released 2026-09-03) exists; `gpu-fft` (CubeCL, radix-2) also exists. Maturity, 2D transforms and f32 accuracy are unverified [V for existence, U for quality].
9. **Section 9.1, Schirninger et al. (2026):** authors and arXiv ID confirmed; the abstract says synthetic MURaM, GREGOR and DKIST tests and that spatially varying PSFs are future work [V]. New 2025 and 2023 items are missing from section 9 (table in section 2 above): Schirninger et al. (2025), Asensio Ramos et al. (2023), Asensio Ramos (2024).
10. **Section 8.5, differential rotation coefficients [M, confirm coefficients].** Sources disagree on the middle coefficient: 2.396 (accepted-average set with negative signs) versus 2.293 (quoted for Snodgrass & Ulrich 1990) [U]. The 0.17 arcsec per minute arithmetic at the equator and disc centre is reproduced by 14.713 deg/day x 960 arcsec [V, my arithmetic].
11. **Section 1.2, Noll 87 per cent.** Supported by the Noll residual coefficients Delta_1 = 1.0299 and Delta_3 = 0.134: (1.0299 - 0.134) / 1.0299 = 0.87 (coefficients from a secondary paper, [arXiv:1901.08438](https://arxiv.org/pdf/1901.08438)) [U for the primary table].
12. **Section 1.3, Fried 1978 constants.** P = 5.6 exp[-0.1557 (D/r0)^2] is quoted in [arXiv:0906.3041](https://arxiv.org/pdf/0906.3041), with worked values about 1 in 9 at D/r0 = 5, 1 in 50 at 6, 1 in 3800 at 8; still only one secondary source, check Fried [U]. Note that the old text's "D/r0 = 6 gives rare good frames" is consistent with 1 in 50.
13. **Section 8.3, Neckel & Labs.** Primary details (30 wavelengths, 303 to 1099 nm, fifth-order polynomials in mu, 7 arcsec limb exclusion, about 0.4 per cent accuracy with possible scattered-light over-correction) corroborated by several secondary papers; coefficient tables still not retrieved [U].
14. **Section 7.3, ImPPG 2.1.0 "dated 2025-11-18".** The releases page shows "18 Nov" without a year. Keep the date but tag [U] on the year [V for the version].
15. **Sections 2.3 and 8.6: Kuhn-Lin-Loranz displaced-Sun flats, SOLARNET and derotation by alt-az field rotation are absent.** Added in sections 6 and 7 above. The old review's derotation section covers only solar rotation; alt-az field rotation (SolHAT, AS) is a separate correction and a practical gap.
16. **Section 8.6, "Microsoft ICE discontinued [M, unverified]".** Still unverified; amateur posts show MS-ICE in current use for H-alpha mosaics [U].
17. **Section 9.3, ONNX Runtime WebGPU provider, `ort`, `tract`, `burn` [M].** Not re-checked; remain [U].
18. **Section 1.5, Denker et al. (2018) "50 Hz"** and **section 6.1 KISIP references** were not re-checked this session; the DKIST VBI burst (80 frames at 30 Hz) is new context [V].

---

## 12. Reference list for this refresh

Verified (opened this session):

- Asensio Ramos et al. (2025), torchmfbd, A&A 703, A269. https://arxiv.org/abs/2505.10639 ; https://github.com/aasensio/torchmfbd
- Asensio Ramos (2026), Marginal MOMFBD. https://arxiv.org/abs/2605.11980
- Löfdahl & Hillberg (2022), MFBD with statistical inclusion of uncorrected high-order modes, A&A 668, A129. https://arxiv.org/abs/2205.13650
- Asensio Ramos, Esteban Pozuelo, Kuckein (2023), Accelerating MFBD via deep learning, Sol. Phys. https://arxiv.org/abs/2306.12078
- Asensio Ramos (2024), spatially variant convolution emulator, A&A 688, A88. https://arxiv.org/abs/2405.09864
- Schirninger et al. (2025), Deep learning image burst stacking. https://arxiv.org/abs/2506.04781
- Schirninger et al. (2026), Neural blind deconvolution. https://arxiv.org/abs/2603.05033
- Spagnoletti et al. (2024), diffusion deconvolution. https://arxiv.org/abs/2411.19158
- Popowicz et al. (2017), Review of image quality measures for solar imaging. https://arxiv.org/abs/1709.09458
- Kamlah et al. (2021), Wavelength dependence of image quality metrics. https://arxiv.org/abs/2011.04346
- Deng et al., Objective image-quality assessment by MFGS. https://arxiv.org/abs/1701.05300 ; https://digitalcommons.njit.edu/fac_pubs/7006
- Haugan & Fredvik, SOLARNET Metadata Recommendations (v4, 2024-03-18). https://arxiv.org/abs/2011.12139 ; https://solarnet-metadata.readthedocs.io/en/stable/
- NSO VBI page. https://nso.atlassian.net/wiki/spaces/PDOI/pages/3534716929 ; Leibniz-KIS VTF pipeline. https://www.leibniz-kis.de/en/news/article/vtf-data-pipeline-prototype/
- Siril: https://siril.org/ ; https://siril.org/download/ ; https://siril.org/download/2025-12-05-siril-1-4-0/ ; https://siril.readthedocs.io/en/latest/preprocessing/multipoint.html
- AutoStakkert!: https://www.autostakkert.com/wp/download/ ; https://www.autostakkert.com/wp/change-notes/
- PSS releases https://github.com/Rolf-Hempel/PlanetarySystemStacker/releases ; ImPPG releases https://github.com/GreatAttractor/imppg/releases ; Registax/waveSharp https://www.astronomie.be/registax ; LuckyStackWorker article https://scopetrader.com/luckystackworker-automates-solar-image-processing/
- SolHAT https://github.com/kmgill/solhat ; solar_ha_processing https://github.com/kmgill/solar_ha_processing ; JSol'Ex https://github.com/melix/astro4j ; https://melix.github.io/astro4j/latest/
- wgsl-fft https://docs.rs/wgsl-fft

Unverified (search snippets or secondary): all items tagged [U] above, notably Zheng 2018 and Xuan 2020 CUDA speckle, Beard et al. 2020 SPIE abstract body, sTools and HiFI+ details, DKIST SPEC-0214 contents, Jarolim 2020, Gao 2023, TMT/DATUM/DeTurb, Chae & Sakurai 2008, Zhu & Milanfar, Kuhn et al. 1991 and follow-ups, Kodaikanal/ChroTel/NISP limb-darkening papers, Snodgrass & Ulrich coefficients, JSol'Ex/INTI details beyond the feature list, amateur colour conventions.
