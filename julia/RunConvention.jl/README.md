# RunConvention.jl

Julia reference library for the koine run convention (`run@1`, `docs/plan-run-convention.md`).
Declare parameters where they are read; get a schema, sourced values, and a run directory
(`run.json`, `params.json`, `events.jsonl`, `outputs.json`, `control/`) that any tool can read.
File-based: no HTTP, no scheduler, no plotting, no knowledge of koine. Dependencies: JSON3, SHA, UUIDs, Dates.

```julia
using RunConvention
@param sink.tau::Float64 = 0.0 unit="s" group="sink" mutable=:never env="QS_SINK_TAU" doc="inner sink relaxation time"
@param io.every::Int = 100 mutable=:always doc="output cadence (steps)"
@param run.zend::Float64 = 1.0 mutable=:restart unit="Myr" doc="end time"
@metric mach unit="" doc="shock Mach number"

maybe_print_schema() && exit()                        # `code --schema`
p   = params()                                        # default -> --params f.json -> env -> --name=value
run = start_run("runs/r1"; problem=(name="sod", version="2", params=Dict()), name="first look",
                on_checkpoint = r -> checkpoint!(r, save_state("runs/r1/ckpt.jls"); step=r.seq))
for step in 1:1000
    step % p.io.every == 0 || continue                # reading marks a parameter as used
    progress!(run; step, time=step*1e3, dt=1e3, fraction=step/1000)
    metric!(run; step, values=Dict("mach" => 1.4))
    poll_control!(run; step) == :stop && break        # control/*.json: set (always only), checkpoint, stop, pause
end
finish!(run; code=0, label="ok")                      # reports declared-but-unread parameters
# later: resume("runs/r2"; from="runs/r1/ckpt.jls", argv=["--run.zend=5.0"])   # child run, lineage recorded
```

`--sink.taau=1` is refused (`UnknownParameterError`, with a suggestion); `relaxed=true` or `--relaxed`
warns instead. `mutable=:never` parameters cannot differ on `resume`; `:restart` and `:always` can;
only `:always` can be set through the control inbox. The key is
`sha256(RFC 8785 canonical JSON of {"problem": …, "params": values})`.
A non-finite float is written as the JSON string `"Infinity"`, `"-Infinity"` or `"NaN"` in
every file, the hashed canonical form included, and a `Float64` parameter reads it back
(JSON has no such number; R-D4.3). A parameter at
its declared default requires nothing (`requires=` is checked only on a value that differs),
whatever source the value came from — a resume reads the parent's file.

Test: `julia --project=. -e 'using Pkg; Pkg.instantiate(); Pkg.test()'`.
