# References

Helia implements published methods. This page lists them, with the place in the
code that uses each. The same citations are in the doc comments of the modules,
under a `# References` heading. Where the code only matches the *documented
behaviour* of another program, the page says so and no code from that program
was used.

A DOI was checked against Crossref (and the arXiv listing for SOLARNET) when this
page was written. An entry marked **to verify** has no DOI or has a detail that
could not be confirmed from a primary source; treat it as a pointer, not a
citation, until it is checked. Corrections are welcome: use
[Feedback](../user-guide/sending-feedback.md).

## Lucky imaging and seeing

* Fried, D. L. (1966). Optical resolution through a randomly inhomogeneous medium
  for very long and very short exposures. *J. Opt. Soc. Am.* 56, 1372-1379.
  [doi:10.1364/JOSA.56.001372](https://doi.org/10.1364/JOSA.56.001372).
  The Fried parameter `r0`. Used by [Seeing estimation](../user-guide/seeing.md).
* Fried, D. L. (1978). Probability of getting a lucky short-exposure image through
  turbulence. *J. Opt. Soc. Am.* 68, 1651-1658.
  [doi:10.1364/JOSA.68.001651](https://doi.org/10.1364/JOSA.68.001651).
  Why keeping only the sharpest frames works. Used by frame scoring and selection
  (`quality`).
* Law, N. M., Mackay, C. D., Baldwin, J. E. (2006). Lucky imaging: high angular
  resolution imaging in the visible from the ground. *Astron. Astrophys.* 446,
  739-745. [doi:10.1051/0004-6361:20053695](https://doi.org/10.1051/0004-6361:20053695).
* von der Luehe, O. (1984). Estimating Fried's parameter from a time series of an
  arbitrary resolved object. *J. Opt. Soc. Am. A* 1, 510-519.
  [doi:10.1364/josaa.1.000510](https://doi.org/10.1364/josaa.1.000510).
  The spectral ratio (`seeing::spectral`).
* Scharmer, G. B., van Werkhoven, T. I. M. (2010). S-DIMM+ height characterization
  of day-time seeing using solar granulation. *Astron. Astrophys.* 513, A25.
  [doi:10.1051/0004-6361/200913791](https://doi.org/10.1051/0004-6361/200913791).
  The alignment point motion index (`seeing::motion`).
* Noll, R. J. (1976). Zernike polynomials and atmospheric turbulence.
  *J. Opt. Soc. Am.* 66, 207-211.
  [doi:10.1364/JOSA.66.000207](https://doi.org/10.1364/JOSA.66.000207).
  The Kolmogorov phase spectrum of the test screens.
* Lane, R. G., Glindemann, A., Dainty, J. C. (1992). Simulation of a Kolmogorov
  phase screen. *Waves in Random Media* 2, 209-224.
  [doi:10.1088/0959-7174/2/3/003](https://doi.org/10.1088/0959-7174/2/3/003).
  FFT phase screens with subharmonics (`seeing::screen`).

## Registration, resampling and mosaics

* Kuglin, C. D., Hines, D. C. (1975). The phase correlation image alignment
  method. *Proc. IEEE Int. Conf. on Cybernetics and Society*, 163-165.
  **To verify** (no DOI; page range from memory). Global alignment (`align::global`).
* Cooley, J. W., Tukey, J. W. (1965). An algorithm for the machine calculation of
  complex Fourier series. *Math. Comput.* 19, 297-301.
  [doi:10.2307/2003354](https://doi.org/10.2307/2003354). The mixed-radix FFT in
  the GPU registration kernels.
* Harris, F. J. (1978). On the use of windows for harmonic analysis with the
  discrete Fourier transform. *Proc. IEEE* 66, 51-83.
  [doi:10.1109/PROC.1978.10837](https://doi.org/10.1109/PROC.1978.10837).
  The Hann window.
* Beaton, A. E., Tukey, J. W. (1974). The fitting of power series, meaning
  polynomials, illustrated on band-spectroscopic data. *Technometrics* 16,
  147-185. [doi:10.1080/00401706.1974.10489171](https://doi.org/10.1080/00401706.1974.10489171).
  The biweight of the limb fit (`align::limb`).
* Duchon, C. E. (1979). Lanczos filtering in one and two dimensions.
  *J. Appl. Meteorol.* 18, 1016-1022.
  [doi:10.1175/1520-0450(1979)018<1016:LFIOAT>2.0.CO;2](https://doi.org/10.1175/1520-0450(1979)018%3C1016:LFIOAT%3E2.0.CO;2).
  The Lanczos resampling kernel.
* Padfield, D. (2012). Masked object registration in the Fourier domain.
  *IEEE Trans. Image Process.* 21, 2706-2718.
  [doi:10.1109/TIP.2011.2181402](https://doi.org/10.1109/TIP.2011.2181402).
  Mosaic panel search (`mosaic::pair`).
* Lucas, B. D., Kanade, T. (1981). An iterative image registration technique with
  an application to stereo vision. *Proc. 7th Int. Joint Conf. on Artificial
  Intelligence*, 674-679. No DOI. Mosaic panel refinement.
* Avidan, S., Shamir, A. (2007). Seam carving for content-aware image resizing.
  *ACM Trans. Graph.* 26(3), 10.
  [doi:10.1145/1276377.1276390](https://doi.org/10.1145/1276377.1276390).
  The least-difference seam of a mosaic (`mosaic::blend`).
* Fruchter, A. S., Hook, R. N. (2002). Drizzle: a method for the linear
  reconstruction of undersampled images. *Publ. Astron. Soc. Pac.* 114, 144-152.
  [doi:10.1086/338393](https://doi.org/10.1086/338393). The drizzle deposit; the
  interface exists and the method is not implemented yet.

## Statistics and robust estimation

* Huber, P. J. (1964). Robust estimation of a location parameter. *Ann. Math.
  Statist.* 35, 73-101.
  [doi:10.1214/aoms/1177703732](https://doi.org/10.1214/aoms/1177703732).
* Huber, P. J., Ronchetti, E. M. (2009). *Robust Statistics*, 2nd ed. Wiley.
  [doi:10.1002/9780470434697](https://doi.org/10.1002/9780470434697). The Huber
  step and winsorising in robust stacking (`stack`), the Huber weight in the
  mosaic solve and the self-calibrated flat.
* Hampel, F. R. (1974). The influence curve and its role in robust estimation.
  *J. Am. Stat. Assoc.* 69, 383-393.
  [doi:10.1080/01621459.1974.10482962](https://doi.org/10.1080/01621459.1974.10482962).
  The median absolute deviation as a noise scale.
* Rousseeuw, P. J., Croux, C. (1993). Alternatives to the median absolute
  deviation. *J. Am. Stat. Assoc.* 88, 1273-1283.
  [doi:10.1080/01621459.1993.10476408](https://doi.org/10.1080/01621459.1993.10476408).

## Calibration and demosaicing

* Kuhn, J. R., Lin, H., Loranz, D. (1991). Gain calibrating nonuniform image-array
  data using only the image data. *Publ. Astron. Soc. Pac.* 103, 1097-1108.
  [doi:10.1086/132932](https://doi.org/10.1086/132932).
  Self-calibrated flats (`calibrate::selfflat`).
* Malvar, H. S., He, L.-W., Cutler, R. (2004). High-quality linear interpolation
  for demosaicing of Bayer-patterned color images. *Proc. IEEE ICASSP 2004*, vol. 3,
  iii-485. [doi:10.1109/ICASSP.2004.1326587](https://doi.org/10.1109/ICASSP.2004.1326587).
  The `mhc` demosaicing.

## Sharpening and deconvolution

* Holschneider, M., Kronland-Martinet, R., Morlet, J., Tchamitchian, P. (1989). A
  real-time algorithm for signal analysis with the help of the wavelet transform.
  In *Wavelets: Time-Frequency Methods and Phase Space*, Springer, 286-297.
  [doi:10.1007/978-3-642-75988-8_28](https://doi.org/10.1007/978-3-642-75988-8_28).
  The a-trous ("with holes") algorithm.
* Starck, J.-L., Murtagh, F. (1994). Image restoration with noise suppression
  using a wavelet transform and a multiresolution support constraint. *Proc. SPIE*
  2302, 132-143. [doi:10.1117/12.188035](https://doi.org/10.1117/12.188035).
  Wavelet layers with a noise threshold per layer. (The same authors' article in
  *Astron. Astrophys.* 288, 342 (1994) has no DOI: **to verify** the bibcode
  1994A&A...288..342S before citing it.)
* Starck, J.-L., Murtagh, F., Bijaoui, A. (1998). *Image Processing and Data
  Analysis: The Multiscale Approach*. Cambridge University Press (ISBN **to verify**).
  The textbook treatment of the a-trous transform with the B3 spline.
* Donoho, D. L. (1995). De-noising by soft-thresholding. *IEEE Trans. Inf. Theory*
  41, 613-627. [doi:10.1109/18.382009](https://doi.org/10.1109/18.382009).
* Richardson, W. H. (1972). Bayesian-based iterative method of image restoration.
  *J. Opt. Soc. Am.* 62, 55-59.
  [doi:10.1364/JOSA.62.000055](https://doi.org/10.1364/JOSA.62.000055).
* Lucy, L. B. (1974). An iterative technique for the rectification of observed
  distributions. *Astron. J.* 79, 745.
  [doi:10.1086/111605](https://doi.org/10.1086/111605).
* Dey, N., Blanc-Feraud, L., Zimmer, C., Roux, P., Kam, Z., Olivo-Marin, J.-C.,
  Zerubia, J. (2006). Richardson-Lucy algorithm with total variation
  regularization for 3D confocal microscope deconvolution. *Microsc. Res. Tech.*
  69, 260-266. [doi:10.1002/jemt.20294](https://doi.org/10.1002/jemt.20294).
  Deconvolution (`edit::deconv`).
* Lupton, R., Blanton, M. R., Fekete, G., Hogg, D. W., O'Mullane, W., Szalay, A.,
  Wherry, N. (2004). Preparing red-green-blue images from CCD data. *Publ. Astron.
  Soc. Pac.* 116, 133-137. [doi:10.1086/382245](https://doi.org/10.1086/382245).
  The `asinh` stretch of the prominence layer.

## Frame quality metrics

* Popowicz, A., Radlak, K., Bernacki, K., Orlov, V. (2017). Review of image
  quality measures for solar imaging. *Solar Phys.* 292, 187.
  [doi:10.1007/s11207-017-1211-3](https://doi.org/10.1007/s11207-017-1211-3).
* Helmli, F. S., Scherer, S. (2001). Adaptive shape from focus with an error
  estimation in light microscopy. *Proc. 2nd Int. Symp. on Image and Signal
  Processing and Analysis (ISPA 2001)*, 188-193.
  [doi:10.1109/ISPA.2001.938626](https://doi.org/10.1109/ISPA.2001.938626).
  The Helmli-Scherer measure (`quality_extra`). The DCT energy ratio is after Su
  et al. as cited by Popowicz et al.; the original Su et al. reference is **to
  verify**. The gradient-energy and Laplacian-variance metrics are textbook
  measures with no single source.

## The Sun

* Meeus, J. (1998). *Astronomical Algorithms*, 2nd ed. Willmann-Bell, Richmond,
  Virginia. ISBN 978-0-943396-61-3 (**to verify**). Chapter 14 (parallactic angle,
  field rotation), chapters 25 and 29 (the Sun's position, `P`, `B0`, `L0`).
* Thompson, W. T. (2006). Coordinate systems for solar image data. *Astron.
  Astrophys.* 449, 791-803.
  [doi:10.1051/0004-6361:20054262](https://doi.org/10.1051/0004-6361:20054262).
* Neckel, H., Labs, D. (1994). Solar limb darkening 1986-1990 (lambda 303 to
  1099 nm). *Solar Phys.* 153, 91-114.
  [doi:10.1007/BF00712494](https://doi.org/10.1007/BF00712494).
* Cox, A. N. (ed.) (2000). *Allen's Astrophysical Quantities*, 4th ed. Springer
  (ISBN **to verify**). The quadratic limb-darkening law.
* Howard, R. F., Harvey, J. W., Forgach, S. (1990). Solar surface velocity fields
  determined from small magnetic features. *Solar Phys.* 130, 295-311.
  [doi:10.1007/BF00156795](https://doi.org/10.1007/BF00156795).
* Snodgrass, H. B., Ulrich, R. K. (1990). Rotation of Doppler features in the solar
  photosphere. *Astrophys. J.* 351, 309.
  [doi:10.1086/168467](https://doi.org/10.1086/168467).
* Howard, R. (1984). Solar rotation. *Annu. Rev. Astron. Astrophys.* 22, 131-155.
  [doi:10.1146/annurev.aa.22.090184.001023](https://doi.org/10.1146/annurev.aa.22.090184.001023).
  Differential rotation (`derotate`).
* Clette, F., Svalgaard, L., Vaquero, J. M., Cliver, E. W. (2014). Revisiting the
  sunspot number. *Space Sci. Rev.* 186, 35-103.
  [doi:10.1007/s11214-014-0074-2](https://doi.org/10.1007/s11214-014-0074-2).
  The relative sunspot (Wolf) number `R = k (10 g + s)`.

## File formats and metadata

* Pence, W. D., Chiappetti, L., Page, C. G., Shaw, R. A., Stobie, E. (2010).
  Definition of the Flexible Image Transport System (FITS), version 3.0. *Astron.
  Astrophys.* 524, A42.
  [doi:10.1051/0004-6361/201015362](https://doi.org/10.1051/0004-6361/201015362).
  The block layout, header cards, `BITPIX`, `BZERO`/`BSCALE` scaling, image
  extensions and the bottom-row-first order that `helia stack` follows when it
  reads FITS frames (`io::fits`, `io::fitsseq`). `BAYERPAT` and `ROWORDER` are
  not in the standard; they are the convention of capture programs, described in
  [Stack FITS frames](../tutorials/stack-fits-frames.md#6-how-helia-reads-the-header).
* Greisen, E. W., Calabretta, M. R. (2002). Representations of world coordinates in
  FITS. *Astron. Astrophys.* 395, 1061-1075.
  [doi:10.1051/0004-6361:20021326](https://doi.org/10.1051/0004-6361:20021326).
* Calabretta, M. R., Greisen, E. W. (2002). Representations of celestial
  coordinates in FITS. *Astron. Astrophys.* 395, 1077-1122.
  [doi:10.1051/0004-6361:20021327](https://doi.org/10.1051/0004-6361:20021327).
* SOLARNET metadata recommendations for solar observations (Haugan, Fredvik and
  co-authors), [arXiv:2011.12139](https://arxiv.org/abs/2011.12139).
* Hahn, G. SER file format. Cited in the research review; the specification is on
  the author's web page.

## Plugins and interchange

* JSON-RPC Working Group (2013). *JSON-RPC 2.0 Specification*.
  [jsonrpc.org/specification](https://www.jsonrpc.org/specification). The message format
  between Helia and a plugin (`helia-plugin`, `helia-plugin-sdk::wire`). A standards document with no DOI.
* Bray, T. (ed.) (2017). *The JavaScript Object Notation (JSON) Data Interchange Format.* RFC 8259.
  [doi:10.17487/RFC8259](https://doi.org/10.17487/RFC8259). The text of every message.
* Preston-Werner, T., Gedam, P. et al. (2021). *TOML v1.0.0*.
  [toml.io/en/v1.0.0](https://toml.io/en/v1.0.0). The format of `plugin.toml`. A standards
  document with no DOI.
* Wright, A., Andrews, H., Hutton, B., Dennis, G. (2022). *JSON Schema: A Media Type for
  Describing JSON Documents* (draft 2020-12).
  [json-schema.org](https://json-schema.org/draft/2020-12/json-schema-core). The subset behind
  the settings form and the step parameters of a plugin (`helia-plugin::schema`).
* National Institute of Standards and Technology (2015). *Secure Hash Standard (SHS)*. FIPS
  PUB 180-4. [doi:10.6028/NIST.FIPS.180-4](https://doi.org/10.6028/NIST.FIPS.180-4). SHA-256, the
  checksum a plugin is approved by.
* Josefsson, S., Liusvaara, I. (2017). *Edwards-Curve Digital Signature Algorithm (EdDSA)*.
  RFC 8032. [doi:10.17487/RFC8032](https://doi.org/10.17487/RFC8032). The key type named in the
  `[update]` section of a manifest; Helia does not verify signatures in API version 1.
* LLVM Project. *libFuzzer: a library for coverage-guided fuzz testing*.
  [llvm.org/docs/LibFuzzer.html](https://llvm.org/docs/LibFuzzer.html). The fuzzing engine behind
  the `plugin_protocol` target. Project documentation with no DOI.

## Behaviour matched from documentation (no code used)

Helia's wavelet controls follow the *documented* behaviour and value ranges of
RegiStax 6, taken from the author's published pages and from the tutorials listed
in [Coming from RegiStax](../user-guide/coming-from-registax.md). RegiStax is
closed source and no code, binary or private information was used. The
prominence layer reads in the spirit of the separate disc and prominence
adjustments that tools such as ImPPG offer; ImPPG's public README was read to
learn what users expect, and none of its code was read or used. The research
reviews under [Research](../developer/research.md) compare Helia with other
programs from their public documentation.

See also [Credits, licences and trademarks](credits-and-trademarks.md).
