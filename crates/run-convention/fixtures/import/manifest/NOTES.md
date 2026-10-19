# 20260702-cicass-suite512-paired-fixed-m5

**Goal:** 512^3 CICASS fixed-amplitude paired suite over four box sizes, with two
seed pairs and inline angle-resolved `P(k,mu)` plus PDFs.

**Machine:** `host-a`

**Code state:**

- Vespa.jl: `1a5587f` plus local inline-PDF change in `patch_cicass.jl`
- CICASS.jl: `6f710e5`
- cicass C checkout: `f7713cc`, rebuilt locally so `libcicass_capi.dylib`
  accepts `-F` fixed-amplitude and `-I` phase-flip flags

**Bulk data:** `/home/user/Projects/cicass_suite512_paired_fixed`

**Tracked small artifacts:**

- `manifest.tsv`
- `suite_run_summary.tsv`
- `suite_box_summary.tsv`
- `suite_performance_summary.md`

**Result:** Full 16-run suite completed and assembled. The combined HDF5 is in the
bulk directory as `suite512_assembled.h5`; it has seven redshift groups, 8192
stitched `P(k,mu)` rows per redshift, and PDF stacks for all four boxes.

**Important setup note:** The first launch exposed a stale local CICASS dylib that
printed `Bad option -F1` and `Bad option -I0` while still writing an IC. That run
was interrupted, `cicass` was merged with `origin/main`, the C ABI dylib was
rebuilt, fixed/flipped IC generation was smoke-tested, and the suite was rerun
with `CIC_SUITE_OVERWRITE_IC=1`.
