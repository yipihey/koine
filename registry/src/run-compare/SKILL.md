---
name: run-compare
description: Compare a verb's step timings between two lab-notebook runs in a koine app, to find which step got slower. Use when a person asks why something got slower, or to check a change against a baseline run.
---

# Run compare

1. `notebook-service_runs` lists runs; note the ids of the baseline run (A) and the new one (B).
2. `notebook-service_compare {"verb": "<verb>", "run_a": "<A>", "run_b": "<B>"}` answers one row per step: count, mean, p50, p95 of total time, mean self time, and `ratio_p50` (B over A).
3. A step with a high `ratio_p50` and a high `b_self_mean_ms` is where the time went. Open one of its calls with `notebook-service_entry` to see its step tree, and record what you found with `notebook-service_annotate`.
4. The "Run compare" surface shows the same comparison to the person.
