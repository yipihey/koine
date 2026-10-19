# Resume from a checkpoint as a child run (R-D4.2 lineage, R-D2.3 classes).

"""
    resume(dir; from, argv=ARGS, file=nothing, env=ENV, relaxed=false, name=nothing,
           registry=REGISTRY, on_checkpoint=nothing, step=nothing) -> Run

Open `dir` as a new run whose `lineage.parent` names the run owning checkpoint `from`
(run id, checkpoint path, sha256). The parameters start from the parent's `params.json`,
then environment and command line apply on top. A changed `never` parameter is refused
(`ImmutableParameterError`); a changed `restart` parameter is applied and recorded as a
`changed` event with `by: "restart"`. The problem is inherited from the parent.
"""
function resume(dir::AbstractString; from::AbstractString, argv = ARGS, file = nothing, env = ENV,
                relaxed::Bool = false, name = nothing, registry::Registry = REGISTRY,
                on_checkpoint = nothing, step = nothing)
    ck = abspath(from)
    isfile(ck) || throw(ArgumentError("checkpoint $from does not exist"))
    pdir = dirname(ck)
    while !isfile(joinpath(pdir, "run.json"))
        up = dirname(pdir)
        up == pdir && throw(ArgumentError("no run.json found above checkpoint $from"))
        pdir = up
    end
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
    rel = startswith(ck, normpath(abspath(pdir)) * "/") ? ck[length(normpath(abspath(pdir)))+2:end] : ck
    pstep = step
    if pstep === nothing
        for o in read_json(joinpath(pdir, "outputs.json"))["outputs"]
            o["path"] == rel && haskey(o, "step") && (pstep = o["step"])
        end
    end
    lineage = Dict{String,Any}("parent" => Dict{String,Any}("run" => parent["id"], "relation" => "resume",
                                   "checkpoint" => Dict{String,Any}("path" => rel, "sha256" => sha256_file(ck))))
    run = start_run(dir; problem = parent["problem"],
                    name = name === nothing ? get(parent, "name", nothing) : name,
                    params = p, lineage, on_checkpoint)
    for (n, old, new) in changed
        emit!(run, "changed", "param" => n, "from" => old, "to" => new, "by" => "restart", "step" => pstep)
    end
    run
end
