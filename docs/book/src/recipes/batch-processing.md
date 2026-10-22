# Batch processing on the command line

**Goal:** stack a whole session, or many captures, without opening the app.

Every `helia stack` run needs only an input and an output. Helia reads the filter
from each file name, so with names such as `Sun_Ha_001.ser` and `Sun_CaK_002.ser`
one command suits both and each gets its own recommended settings.

## A folder of captures

In a POSIX shell (Linux, macOS, or Git Bash on Windows):

```sh
mkdir -p stacks
for f in captures/*.ser; do
  helia stack "$f" -o "stacks/$(basename "$f" .ser).tif" --sharpen wavelet --quiet \
    || echo "failed: $f"
done
```

In PowerShell:

```powershell
New-Item -ItemType Directory -Force stacks | Out-Null
Get-ChildItem captures\*.ser | ForEach-Object {
  helia stack $_.FullName -o "stacks\$($_.BaseName).tif" --sharpen wavelet --quiet
  if ($LASTEXITCODE -ne 0) { Write-Warning "failed: $($_.Name)" }
}
```

Both carry on after a failing capture, so one bad file does not hide the others.
`--quiet` hides the progress bar and the summary; the exit status is non-zero on
failure. Leave `--quiet` off to keep each run's summary in a log:

```sh
helia stack "$f" -o "stacks/$(basename "$f" .ser).tif" > "logs/$(basename "$f" .ser).txt" 2>&1
```

## Test the batch on one file first

```sh
helia info captures/Sun_Ha_001.ser     # what Helia will use, and any notice
helia stack captures/Sun_Ha_001.ser -o test.tif --sharpen wavelet
```

## Other formats and options

* A PNG for sharing and a FITS for measurement from the same capture: run it twice,
  with `-o x.png --preset ha` and `-o x.fits`.
* All sharpening, colour and alignment options work in a batch; see the
  [CLI reference](../reference/cli.md).
* Make the same edits to existing images with `helia edit`, with a saved preset:

```sh
for f in stacks/*.tif; do
  helia edit "$f" -o "edited/$(basename "$f")" --edits "H-alpha gentle" --quiet
done
```

## One at a time, or all at once

The GPU is one device, so running several `helia stack` jobs at once rarely helps.
Run them in turn, as above. If you want a queue that other programs can add to,
use the [API](scripting-with-the-api.md), which runs one job at a time by default.
