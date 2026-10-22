# New-method verification, October 2026

Scope: eleven candidate methods proposed from memory were checked against primary or official sources with web search and page fetches, then filed as issues or added as comments on existing issues. Tags: [V] verified this session (abstract or description unless stated), [U] unverified. WebFetch summarises pages with a small model, so anything that drives a design decision should be re-read at source before it is built. Many classical papers (Sarazin and Roddier 1990, Korff 1973, Kuhn et al. 1991, Huber 1964, von der Luehe 1984) could not be retrieved in full and are known here from abstracts or secondary papers.

## Outcome

| # | Candidate | Result | Where |
|---|---|---|---|
| 1 | r0 / seeing from the data | Filed, with a correction (below) | #49, M3 |
| 2 | Fourier lucky imaging | Filed as an experiment; evidence is simulation and stellar | #50, M6 |
| 3 | STF "speckle-lite" | Filed as an experiment, with a correction (below) | #51, M6 |
| 4 | Deconvolution by the telescope OTF | Commented on #5 (Airy kernel already planned there) | #5 |
| 5 | TV-regularised, limb-aware RL | Commented on #5 | #5 |
| 6 | Kuhn-Lin-Loranz flats | Filed (was "later" in the matrix with no issue) | #52, M2 |
| 7 | Robust M-estimator stacking | Filed | #53, M2 |
| 8 | Dense optical flow / destretching | Filed as a harness experiment; linked to #4, not #26 | #54, M6 |
| 9 | Spectroheliograph mode | Filed as an epic in a new milestone | #55, M7 |
| 10 | Science outputs | Split into granulation statistics (#56, M3), sunspots and Wolf number (#57, M4), tracking, rotation and light curves (#58, M6) | |
| 11 | SDO reference registration | Filed, explicit opt-in | #59, M4 |

Cross-reference comments were added to #3, #4, #5, #15, #19, #21, #25 and #26.

## Corrections to the proposals

1. **r0 from alignment-point shifts is not DIMM.** The Sarazin and Roddier DIMM formulae relate the variance of differential image motion between two sub-apertures of one pupil to r0 [V via secondary papers]. Helia's AP shifts are differential motion across the field through one aperture, which depends on the turbulence height profile (the S-DIMM+ regime of Scharmer and van Werkhoven 2010 [V]). It is therefore a relative index; the spectral-ratio method of von der Luehe (1984) is the route to an absolute r0 [V description].
2. **Fourier lucky imaging evidence.** Garrel, Guyon and Baudoz (2012) is simulation of an 8 m AO telescope at 0.6 arcsec seeing, point-like targets [V abstract]. Mackay (2013) tested it on stellar data [V abstract]. No solar or extended-scene result found [U]. Their algorithm details were not read.
3. **STF is not applied to a stack spectrum.** In Labeyrie-type reconstruction (the KISIP lineage, Woeger, von der Luehe and Reardon 2008 [V abstract]) the average power spectrum of the raw burst is divided by the speckle transfer function, the average of the squared OTF, with Korff (1973) giving the Kolmogorov form from r0 [V via review]. A shift-and-add stack has a different transfer function (a mean OTF under the alignment and selection policy), so the useful Helia version needs a Monte Carlo-calibrated model. No amateur or planetary software using an STF was found [U].
4. **#5 scope.** The issue text lists unsharp masking and a-trous wavelets; Richardson-Lucy appears only as "planned #5" in the method matrix. TV-RL is established (Dey et al. 2006 [V]; Siril offers TV or Hessian regularisation [V]; JSol'Ex ships RL [V]). The central obstruction changes the OTF shape and ring energy but not the D/lambda cutoff of the autocorrelation model [U for closed forms].
5. **Optical flow link.** The closest existing issue is #4 (shift-field smoothing), not #26 (mosaics and derotation). The existing review already recommended testing TV-L1 in the harness only; no solar comparison was found [U].
6. **Spectroheliography.** The JSol'Ex user guide names Sol'Ex, the SHG 700 and SunScan; it does not mention MLAstro SHG [V]. JSol'Ex is Apache-2.0 [V]; implement independently and attribute anything copied.
7. **Differential rotation from granules is infeasible.** Granulation evolves in minutes; rotation needs long-lived tracers (spots, filaments). Reflected in #58.
8. **Robust stacking is a guard, not a gain.** Seeing-selected stacks are not outlier-contaminated Gaussians; the weighted mean is already the model's maximum-likelihood estimate (#53).

## Verified facts used

- Siril rejection algorithms include Winsorized sigma clipping (crediting Huber), GESD, linear fit, MAD and percentile clipping [V, https://siril.readthedocs.io/en/stable/preprocessing/stacking.html].
- Kroeger et al. (2016), DIS: https://arxiv.org/abs/1603.03590 [V]. Zach, Pock and Bischof (2007), TV-L1, DAGM LNCS 4713, 214 [V listing].
- Fisher and Welsch (2008), FLCT: https://arxiv.org/abs/0712.4289 [V].
- Kanzelhoehe pipeline describes the Kuhn-Lin-Loranz flat (shifted Sun images, assumes a static Sun, manual capture): https://arxiv.org/pdf/2111.03176 [V].
- JSol'Ex features (line search, polynomial distortion correction, ellipse tilt and ratio fit, pixel-shift reconstruction, Doppler and continuum products, RL, CLAHE, stripe correction, FITS, load_sdo): https://melix.github.io/astro4j/latest/en/jsolex.html [V].
- HMI continuum is derived from the Fe I 6173 A line fit; JSOC and sunpy Fido are the standard route [V listing].

## Still unverified

Full text of: Garrel et al. 2012, Kuhn et al. 1991, Sarazin and Roddier 1990, Korff 1973, von der Luehe 1984 and 1993, Woeger et al. 2008. Wolf number definition and SILSO scaling (general knowledge). Helioviewer endpoint names, rate limits and terms of use. Granule size and contrast values. INTI algorithm details. Analytic annular-pupil OTF forms.
