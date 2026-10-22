# The GPU parity rule

> Every GPU kernel has a CPU reference implementation, and a test that the two
> agree within a stated tolerance. The CPU path is the specification.

This is the rule in [Contributing](https://github.com/qisback/helia/blob/main/CONTRIBUTING.md); this page says how it works.

## Why

A GPU result you cannot check is a result you cannot trust. Helia needs the same
stack from an NVIDIA card, a Raspberry Pi, an Apple GPU and a laptop with no GPU
at all. The CPU implementation is simple enough to read and test against known
answers (a known shift, a known blur), so it defines the correct result, and the
GPU is tested against it. The `Backend` trait lives in `helia-core`, so the core
builds and tests with no GPU dependency.

## What the tests compare

`crates/helia-gpu/tests/parity.rs` runs the CPU and GPU implementations on the
same synthetic data:

| Test | Compares | Tolerance |
|---|---|---|
| `accumulate_matches_cpu` | Accumulated sums and validity maps of shift-and-add | sums within 1e-4 per unit weight; validity maps exactly equal |
| `sharpness_matches_cpu` | Frame sharpness scores | within 2e-3 relative |
| `colourise_matches_the_cpu_reference` | Colouring a mono image through every preset, inverted and not, at full and partial strength, including non-finite samples | within 1e-5 per sample (16-bit steps are 1.5e-5) |
| `tone_edits_match_the_cpu_reference` | Levels (with and without gamma) and a tone curve | within 2e-6 (the same arithmetic; `pow` and the cubic differ in the last bit) |
| `unsharp_and_wavelet_edits_match_the_cpu_reference` | Unsharp mask, a six-layer wavelet transform with noise cuts, and denoise, on a clean and a noisy image; the all-default wavelet must return its input exactly | unsharp within 2e-5, wavelets and denoise within 1e-4 |
| `deconvolution_matches_the_cpu_reference` | Richardson-Lucy with a Gaussian and with an Airy PSF, with and without the limb guard, and with an automatic PSF | within 2e-3 after 8 to 12 iterations |
| `a_mixed_list_with_geometry_matches_the_cpu_reference` | A list of sharpening, crop, rotate, flip, levels and resize: the device runs the filters, the CPU the geometry | within 1e-4 |
| `pipeline_on_gpu_matches_cpu` | A whole global-only stack through the pipeline (half the frames kept, Lanczos3, not normalised) | stack RMSE below 1e-4; identical frame ranking |
| `backend_selection_never_fails_for_cpu_and_auto` | `cpu` and `auto` always yield a backend | n/a |
| `drizzle_is_unsupported_on_gpu` | Drizzle returns `Unsupported`, as on the CPU | n/a |

The edit parity tests (`edits_match`) print the observed worst difference; for
the wavelets and the unsharp mask it is 0 on the software adapter, because the
convolutions add their taps in the same order as the CPU, and for deconvolution
about 1e-6. The observed differences are several orders tighter (about 1e-6 and 1e-7)
because the GPU kernels use the CPU's tap weights and the same order of
operations. The stated tolerances leave room for floating-point contraction and
for the GPU summing per workgroup in `f32` where the CPU sums in `f64`, and they
are written next to the tests rather than hidden.

## Rules for a new kernel

1. Write the CPU version first, in `helia-core`, with unit tests on synthetic
   data.
2. Add the GPU version in `helia-gpu`, behind the same trait method.
3. Add a parity test that runs both and states its tolerance, with the reason
   for the figure.
4. Run it under `HELIA_GPU_TESTS=software` (so it runs anywhere) and, where you
   can, under `hardware`.
5. Update [GPU acceleration](../user-guide/gpu.md) and the
   [Architecture](architecture.md) page, which list what runs where.

## Running the parity tests

```sh
HELIA_GPU_TESTS=software cargo test -p helia-gpu
```

See [the source repository](https://github.com/qisback/helia) for what `software`,
`hardware` and `none` mean.
