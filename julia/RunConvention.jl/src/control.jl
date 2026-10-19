# The control inbox (R-D8): control/<id>.json -> acted on -> control/done/ or control/refused/.

function refuse!(run::Run, file, id, req, reason)
    d = joinpath(run.dir, "control", "refused"); mkpath(d)
    doc = req isa AbstractDict ? Dict{String,Any}(req) : Dict{String,Any}("raw" => string(req))
    doc["reason"] = reason
    write_json_atomic(joinpath(d, id * ".json"), doc)
    rm(file; force = true)
    note!(run, "control request refused"; data = Dict{String,Any}("id" => id, "reason" => reason))
end

function done!(run::Run, file, id)
    d = joinpath(run.dir, "control", "done"); mkpath(d)
    mv(file, joinpath(d, id * ".json"); force = true)
end

function apply_set!(run::Run, id, req, step)
    p = run.params
    name = get(req, "param", nothing)
    name isa AbstractString || return "set needs a string \"param\""
    haskey(req, "value") || return "set needs a \"value\""
    spec = get(p.specs, name, nothing)
    spec === nothing && return "unknown parameter '$name'"
    spec.mutable == "always" || return "parameter '$name' is mutable=:$(spec.mutable), not :always"
    new = try
        v = req["value"]
        v isa AbstractString && !(spec.type <: AbstractString) ? parse_text(spec.type, v, name) :
            coerce_value(spec.type, v, name)
    catch err
        err isa ParameterValueError ? err.msg : rethrow()
    end
    new isa AbstractString && !(spec.type <: AbstractString) && return new
    try
        check_constraints(spec, new)
    catch err
        err isa ParameterValueError ? (return err.msg) : rethrow()
    end
    lock(p.lock) do
        old = p.values[name]
        p.values[name] = new
        p.sources[name] = "control:" * id
        emit!(run, "changed", "param" => name, "from" => old, "to" => new, "by" => "control:" * id, "step" => step)
    end
    nothing
end

"""
    poll_control!(run; step=nothing, pause_poll=0.5, pause_timeout=Inf) -> Symbol

Process `control/*.json` in name order. `set` applies only to `mutable=:always` parameters (the
rest go to `control/refused/` with a reason) and records a `changed` event naming `control:<id>`.
`checkpoint` calls the `on_checkpoint(run)` callback given to `start_run` (which should call
`checkpoint!`); with no callback it is refused. `pause` blocks, polling, until a `resume`
or `stop` request arrives. `stop` returns `:stop` (after the callback when `"after": "checkpoint"`).
Returns `:stop` if any stop request was seen, else `:continue`. Handled files move to `control/done/`.
A file modified under 2 s ago that does not parse is left alone: it may still be being written.
"""
function poll_control!(run::Run; step = nothing, pause_poll::Real = 0.5, pause_timeout::Real = Inf)
    dir = joinpath(run.dir, "control")
    isdir(dir) || return :continue
    paused = Ref(false)
    t0 = time()
    while true
        process_control!(run, dir, step, paused) === :stop && return :stop
        paused[] || return :continue
        # Paused: wait for the next request (a resume, a stop, or a set).
        while !any(f -> endswith(f, ".json") && isfile(joinpath(dir, f)), readdir(dir))
            time() - t0 > pause_timeout && return :continue
            sleep(pause_poll)
        end
    end
end

function process_control!(run::Run, dir, step, paused::Ref{Bool})
    for f in sort(filter(x -> endswith(x, ".json") && isfile(joinpath(dir, x)), readdir(dir)))
        file = joinpath(dir, f); id = f[1:end-5]
        req = try
            parse_json(read(file, String))
        catch
            time() - mtime(file) < 2 && continue
            refuse!(run, file, id, "unreadable", "not valid JSON")
            continue
        end
        kind = req isa AbstractDict ? get(req, "request", nothing) : nothing
        if kind == "set"
            reason = apply_set!(run, id, req, step)
            reason === nothing ? done!(run, file, id) : refuse!(run, file, id, req, reason)
        elseif kind == "checkpoint"
            if run.on_checkpoint === nothing
                refuse!(run, file, id, req, "no checkpoint handler registered (start_run(...; on_checkpoint=f))")
            else
                run.on_checkpoint(run); done!(run, file, id)
            end
        elseif kind == "stop"
            if get(req, "after", nothing) == "checkpoint" && run.on_checkpoint !== nothing
                run.on_checkpoint(run)
            end
            note!(run, "stop requested"; step, data = Dict{String,Any}("by" => "control:" * id))
            done!(run, file, id)
            return :stop
        elseif kind == "pause"
            note!(run, "paused"; step, data = Dict{String,Any}("by" => "control:" * id))
            done!(run, file, id)
            paused[] = true
        elseif kind == "resume"
            done!(run, file, id)
            paused[] = false
        else
            refuse!(run, file, id, req, "unsupported request $(repr(kind)) (supported: set, checkpoint, stop, pause, resume)")
        end
    end
    :ok
end
