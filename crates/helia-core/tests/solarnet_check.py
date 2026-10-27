# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Read a Helia FITS file with astropy and sunpy and check its SOLARNET header.

usage: solarnet_check.py FILE EXPECTED.json

Exit status 0 when every check passes, 77 when astropy or sunpy cannot be
imported (the caller decides whether that is a failure), 1 otherwise.
"""
import json
import sys
import warnings

try:
    import astropy.units as u
    import numpy as np
    from astropy.io import fits
    from astropy.time import Time
except ImportError as e:
    print(f"skip: astropy not available ({e})")
    sys.exit(77)
try:
    import sunpy.map
except ImportError as e:
    print(f"skip: sunpy not available ({e})")
    sys.exit(77)

failures = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failures.append(what)


def close(a, b, tol):
    return abs(float(a) - float(b)) <= tol


def main(path, expected_path):
    exp = json.load(open(expected_path))
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        with fits.open(path) as hdul:
            h = hdul[0].header
            data = hdul[0].data
    for key in ("EXTNAME", "SOLARNET", "OBS_HDU", "DATE-BEG", "XPOSURE", "NSUMEXP"):
        check(key in h, f"{key} present")
    check(h["OBS_HDU"] == 1, "OBS_HDU = 1")
    check(h["SOLARNET"] in (0.5, 1.0), "SOLARNET is 0.5 or 1")
    check(h["NSUMEXP"] == exp["nsumexp"], f"NSUMEXP = {exp['nsumexp']}")
    check(close(h["XPOSURE"], exp["xposure"], 1e-9), f"XPOSURE = {exp['xposure']} s")
    check(close(h["TEXPOSUR"], exp["texposur"], 1e-12), "TEXPOSUR")
    check(h["DATE-BEG"] == exp["date_beg"], f"DATE-BEG = {exp['date_beg']}")
    check(h["DATE-END"] == exp["date_end"], f"DATE-END = {exp['date_end']}")
    check("DATE-AVG" in h, "DATE-AVG present")
    check(h["TELESCOP"] == exp["telescop"], "TELESCOP from the SER header")
    check(h["INSTRUME"] == exp["instrume"], "INSTRUME from the SER header")
    check(h["OBSERVER"] == exp["observer"], "OBSERVER from the SER header")
    check(close(h["WAVELNTH"], exp["wavelnth"], 1e-6) and h["WAVEUNIT"] == -9, "WAVELNTH / WAVEUNIT")
    for i, ax in enumerate("XYZ"):
        check(
            close(h[f"OBSGEO-{ax}"], exp["obsgeo"][i], 1e-3),
            f"OBSGEO-{ax} = {exp['obsgeo'][i]:.3f}",
        )
    check(h["PRSTEP1"] == "FRAME-SELECTION", "PRSTEP1 is frame selection")
    steps = [h[f"PRSTEP{n}"] for n in range(1, 20) if f"PRSTEP{n}" in h]
    # Later steps (LIMB-DARKENING, CROPPING, ...) follow the normalisation.
    check(
        "STACKING" in steps and "NORMALISATION" in steps and steps.index("STACKING") < steps.index("NORMALISATION"),
        f"PRSTEP list {steps}",
    )
    check(all(f"PRPROC{n}" in h and f"PRPVER{n}" in h for n in range(1, len(steps) + 1)), "PRPROC/PRPVER for every step")
    check(h["PRPVER1"].startswith("helia "), "PRPVER1 names the Helia version")
    check(h["CTYPE1"] == "HPLN-TAN" and h["CTYPE2"] == "HPLT-TAN", "CTYPE1/2 helioprojective TAN")
    check(h["CUNIT1"] == "arcsec" and h["CUNIT2"] == "arcsec", "CUNIT arcsec")
    check(close(h["CDELT1"], exp["cdelt"], 1e-9), f"CDELT = {exp['cdelt']:.6f} arcsec/pixel")
    check(close(h["CRPIX1"], exp["crpix1"], 1e-3) and close(h["CRPIX2"], exp["crpix2"], 1e-3), "CRPIX from the limb fit")
    check(h["CRVAL1"] == 0 and h["CRVAL2"] == 0 and h["CROTA2"] == 0, "CRVAL 0, CROTA2 0 (orientation unknown)")
    check(data is not None and data.shape == tuple(exp["shape"]), "image shape")

    # sunpy must build a helioprojective map from the file.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        m = sunpy.map.Map(path)
    check(isinstance(m, sunpy.map.GenericMap), f"sunpy.map.Map builds a map ({type(m).__name__})")
    check(m.coordinate_system.axis1 == "HPLN-TAN" and m.coordinate_system.axis2 == "HPLT-TAN", "sunpy: helioprojective TAN")
    check(close(m.scale.axis1.to_value(u.arcsec / u.pix), exp["cdelt"], 1e-9), "sunpy: scale")
    check(close(m.reference_pixel.x.to_value(u.pix), exp["crpix1"] - 1, 1e-3), "sunpy: reference pixel x (0-based)")
    check(close(m.reference_pixel.y.to_value(u.pix), exp["crpix2"] - 1, 1e-3), "sunpy: reference pixel y (0-based)")
    check(close(m.rsun_obs.to_value(u.arcsec), exp["rsun_arc"], 1e-6), "sunpy: rsun_obs is the limb-fit radius")
    check(abs((m.reference_date - Time(exp["date_avg"], scale="utc")).to_value(u.s)) < 0.01, "sunpy: reference date is DATE-AVG")
    # Our date-based solar radius agrees with sunpy's ephemeris.
    from sunpy.coordinates import sun

    eph = sun.angular_radius(m.reference_date).to_value(u.arcsec)
    check(close(eph, exp["rsun_arc"], 0.1), f"RSUN_ARC {exp['rsun_arc']:.3f} within 0.1 arcsec of the ephemeris {eph:.3f}")
    check(close(m.exposure_time.to_value(u.s), exp["xposure"], 1e-9), "sunpy: exposure_time")
    check(m.observatory is None or isinstance(m.observatory, str), "sunpy: observatory readable")
    # The disc centre maps to the Sun centre and the limb to the solar radius.
    cx, cy, r = exp["disc_x"], exp["disc_y_fits"], exp["disc_r"]
    centre = m.wcs.pixel_to_world(cx, cy)
    check(abs(centre.Tx.to_value(u.arcsec)) < 1e-6 and abs(centre.Ty.to_value(u.arcsec)) < 1e-6, "sunpy: disc centre is the Sun centre")
    east = m.wcs.pixel_to_world(cx + r, cy)
    north = m.wcs.pixel_to_world(cx, cy + r)
    for name, c in (("west", east), ("north", north)):
        sep = np.hypot(c.Tx.to_value(u.arcsec), c.Ty.to_value(u.arcsec))
        check(close(sep, exp["rsun_arc"], 0.05), f"sunpy: {name} limb is {sep:.2f} arcsec from centre (solar radius {exp['rsun_arc']:.2f})")
    check(east.Tx.to_value(u.arcsec) > 0 and north.Ty.to_value(u.arcsec) > 0, "sunpy: Tx grows to the right, Ty upwards")
    try:
        obs = m.observer_coordinate
        print(f"info observer: {obs.frame.name}, radius {obs.radius.to(u.au):.4f}")
    except Exception as e:  # observer is derived from OBSGEO or the date
        check(False, f"sunpy: observer_coordinate ({e})")
    try:
        check(close(m.wavelength.to_value(u.nm), exp["wavelnth"], 1e-6), "sunpy: wavelength")
    except Exception as e:
        print(f"info sunpy wavelength not read: {e}")

    print(f"astropy {__import__('astropy').__version__}, sunpy {sunpy.__version__}")
    if failures:
        print(f"{len(failures)} check(s) failed")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
