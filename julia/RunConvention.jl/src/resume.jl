# Resume from a checkpoint as a child run (R-D4.2 lineage, R-D2.3 classes).

"""
    resume(dir; from, argv=ARGS, file=nothing, env=ENV, relaxed=false, name=nothing,
           registry=REGISTRY, on_checkpoint=nothing, step=nothing) -> Run

Open `dir` as a new run whose `lineage.parent` names the run owning checkpoint `from`
(run id, checkpoint path, sha256). The owner is the run that last wrote the file: the
nearest run directory above `from`, unless a run below it (a `run-*/` child that resumed
in place) recorded a `checkpoint` event with the same sha256 — then that child. `parent=`
names the owning run directory outright. The parameters start from the parent's `params.json`,
then environment and command line apply on top. A changed `never` parameter is refused
(`ImmutableParameterError`); a changed `restart` parameter is applied and recorded as a
`changed` event with `by: "restart"`. The problem is inherited from the parent.
"""
function resume(dir::AbstractString; from::AbstractString, argv = ARGS, file = nothing, env = ENV,
                relaxed::Bool = false, name = nothing, registry::Registry = REGISTRY,
                on_checkpoint = nothing, step = nothing, parent = nothing)
    ck = abspath(from)
    isfile(ck) || throw(ArgumentError("checkpoint $from does not exist"))
    sha = sha256_file(ck)
    pdir = parent === nothing ? checkpoint_owner(ck, sha) : abspath(parent)
    isfile(joinpath(pdir, "run.json")) || throw(ArgumentError("$pdir holds no run.json"))
    parent = read_json(joinpath(pdir, "run.json"))
    parent_vals = Dict{String,Any}(k => (v isa AbstractDict ? v["value"] : v)
                                   for (k, v) in read_json(joinpath(pdir, "params.json")))
    files = String[joinpath(pdir, "params.json")]
    file === nothing || append!(files, file isa AbstractString ? [String(file)] : String.(file))
    p = params(; argv, file = files, env, relaxed, registry)
    changed = Tuple{String,Any,Any}[]
    for n in p.order
        haskey(parent_vals, n) || continue
        old = coerce_value(p.specs[n].type, parent_vals[n], n); new = p.values[n]
        isequal(old, new) && continue
        cls = p.specs[n].mutable
        cls == "never" && throw(ImmutableParameterError(n, old, new, cls))
        push!(changed, (n, old, new))
    end
    # Relative to the owning run, so a moved run tree still resolves it (`../ckpt.jls`
    # for a child that rewrote its parent's file).
    rel = relpath(ck, normpath(abspath(pdir)))
    pstep = step
    if pstep === nothing
        for e in read_events(pdir)
            e["kind"] == "checkpoint" && get(e, "sha256", nothing) == sha && haskey(e, "step") && (pstep = e["step"])
        end
    end
    if pstep === nothing
        for o in read_json(joinpath(pdir, "outputs.json"))["outputs"]
            o["path"] == rel && haskey(o, "step") && (pstep = o["step"])
        end
    end
    lineage = Dict{String,Any}("parent" => Dict{String,Any}("run" => parent["id"], "relation" => "resume",
                                   "checkpoint" => Dict{String,Any}("path" => rel, "sha256" => sha)))
    run = start_run(dir; problem = parent["problem"],
                    name = name === nothing ? get(parent, "name", nothing) : name,
                    params = p, lineage, on_checkpoint)
    for (n, old, new) in changed
        emit!(run, "changed", "param" => n, "from" => old, "to" => new, "by" => "restart", "step" => pstep)
    end
    run
end

"""
    checkpoint_owner(ck, sha) -> run directory

The run that last wrote checkpoint `ck`: the nearest run directory above it, or the run
below that one whose `events.jsonl` has a `checkpoint` event carrying `sha` (a child that
resumed in place and rewrote the file), the latest-started such run when several do.
"""
function checkpoint_owner(ck::AbstractString, sha::AbstractString)
    root = dirname(ck)
    while !isfile(joinpath(root, "run.json"))
        up = dirname(root)
        up == root && throw(ArgumentError("no run.json found above checkpoint $ck"))
        root = up
    end
    best, best_started = root, ""
    for (d, dirs, files) in walkdir(root)
        d == root && continue
        "run.json" in files || continue
        any(e -> e["kind"] == "checkpoint" && get(e, "sha256", nothing) == sha, read_events(d)) || continue
        started = string(get(get(read_json(joinpath(d, "run.json")), "status", Dict()), "started_at", ""))
        started >= best_started && ((best, best_started) = (d, started))
    end
    best
end
