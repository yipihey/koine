# The run directory: run.json, params.json, events.jsonl, outputs.json (R-D4, R-D6, R-D7).

const CONVENTION = "run@1"

mutable struct Run
    dir::String
    id::String
    key::String
    record::Dict{String,Any}
    params::Params
    seq::Int
    io::Any
    lock::ReentrantLock
    outputs::Vector{Dict{String,Any}}
    finished::Bool
    on_checkpoint::Union{Nothing,Function}
end
Base.show(io::IO, r::Run) = print(io, "Run(", r.id, " @ ", r.dir, ", seq=", r.seq, r.finished ? ", finished)" : ")")

"Drop keys whose value is `nothing`: an absent optional field is omitted, never null."
compact(d::AbstractDict) = Dict{String,Any}(string(k) => v for (k, v) in d if v !== nothing)

# ---- provenance -------------------------------------------------------------------

function git(dir, args...)
    try
        Sys.which("git") === nothing && return nothing
        cmd = `git -C $dir $(collect(args))`
        out = read(pipeline(cmd; stderr = devnull), String)
        out
    catch
        nothing
    end
end

function code_block(code_dir::AbstractString, name, version)
    proj = Base.active_project()
    pdir = proj === nothing ? nothing : dirname(proj)
    if proj !== nothing && isfile(proj)
        for l in eachline(proj)
            m = match(r"^(name|version)\s*=\s*\"([^\"]*)\"", l)
            m === nothing && continue
            m[1] == "name" && name === nothing && (name = m[2])
            m[1] == "version" && version === nothing && (version = m[2])
        end
    end
    manifest = nothing
    for d in filter(!isnothing, (pdir, code_dir))
        isfile(joinpath(d, "Manifest.toml")) && (manifest = sha256_file(joinpath(d, "Manifest.toml")); break)
    end
    b = Dict{String,Any}("name" => name === nothing ? "unnamed" : String(name),
                         "version" => version === nothing ? "0" : String(version),
                         "julia" => string(VERSION), "lock_sha256" => manifest)
    if Sys.which("git") === nothing || git(code_dir, "rev-parse", "--is-inside-work-tree") === nothing
        b["git_available"] = false
    else
        b["git_available"] = true
        head = git(code_dir, "rev-parse", "HEAD")
        b["commit"] = head === nothing ? nothing : strip(head)
        st = git(code_dir, "status", "--porcelain")
        dirty = st === nothing ? nothing : !isempty(strip(st))
        b["dirty"] = dirty
        diff = dirty === true ? git(code_dir, "diff", "HEAD") : nothing
        b["diff_sha256"] = diff === nothing ? nothing : sha256_hex(diff)
    end
    compact(b)
end

function host_block(p::Params)
    envs = Dict{String,Any}()
    for n in p.order
        e = p.specs[n].env
        e !== nothing && haskey(ENV, e) && (envs[e] = ENV[e])
    end
    for e in ("JULIA_NUM_THREADS", "OMP_NUM_THREADS", "CUDA_VISIBLE_DEVICES")
        haskey(ENV, e) && (envs[e] = ENV[e])
    end
    job = nothing
    for e in ("SLURM_JOB_ID", "PBS_JOBID", "LSB_JOBID")
        haskey(ENV, e) && (job = ENV[e]; break)
    end
    cpus = Sys.cpu_info()
    compact(Dict{String,Any}("hostname" => gethostname(), "os" => string(Sys.KERNEL),
                     "arch" => string(Sys.ARCH), "cpu" => isempty(cpus) ? nothing : cpus[1].model,
                     "cpu_threads" => Sys.CPU_THREADS, "julia_threads" => Threads.nthreads(),
                     "scheduler_job_id" => job,
                     "argv" => String[PROGRAM_FILE; ARGS], "env" => envs))
end

normalize_problem(::Nothing) = Dict{String,Any}("name" => "unnamed", "version" => "0", "params" => Dict{String,Any}())
function normalize_problem(pr)
    get2(k, i) = pr isa AbstractDict ? get(pr, k, get(pr, Symbol(k), nothing)) :
                 pr isa NamedTuple ? get(pr, Symbol(k), nothing) : pr[i]
    ps = get2("params", 3)
    Dict{String,Any}("name" => String(something(get2("name", 1), "unnamed")),
                     "version" => String(something(get2("version", 2), "0")),
                     "params" => ps === nothing ? Dict{String,Any}() : plain_params(ps))
end
plain_params(d::AbstractDict) = Dict{String,Any}(string(k) => v for (k, v) in d)
plain_params(d::NamedTuple) = Dict{String,Any}(string(k) => v for (k, v) in pairs(d))

# ---- events -----------------------------------------------------------------------

function open_events(dir)
    Base.Filesystem.open(joinpath(dir, "events.jsonl"),
        Base.Filesystem.JL_O_WRONLY | Base.Filesystem.JL_O_CREAT | Base.Filesystem.JL_O_APPEND, 0o644)
end

"""
Append one event as one line, with the next gap-free `seq`, via a single `write` (R-D6.6).
Fields whose value is `nothing` are omitted.
"""
function emit!(run::Run, kind::AbstractString, fields::Pair...)
    lock(run.lock) do
        run.finished && error("run $(run.id) is finished; no further events")
        d = Dict{String,Any}("seq" => run.seq + 1, "t" => utc_now(), "kind" => String(kind))
        for (k, v) in fields
            v === nothing || (d[String(k)] = v)
        end
        line = json_string(d) * "\n"
        write(run.io, codeunits(line))
        run.seq += 1
        d
    end
end

# ---- start ------------------------------------------------------------------------

function start_run(dir::AbstractString; problem = nothing, name = nothing, params = nothing,
                   code_dir = nothing, code_name = nothing, code_version = nothing,
                   on_checkpoint = nothing, lineage = nothing)
    p = params === nothing ? (LAST_PARAMS[] === nothing ? RunConvention.params() : LAST_PARAMS[]) : params
    dir = String(dir)
    isfile(joinpath(dir, "run.json")) && throw(ArgumentError("$dir already holds a run (run.json exists)"))
    mkpath(joinpath(dir, "control"))
    pr = normalize_problem(problem)
    vals = values_of(p)
    key = run_key(pr, vals)
    id = string(UUIDs.uuid4())
    cdir = code_dir === nothing ? (isempty(PROGRAM_FILE) ? pwd() : dirname(abspath(PROGRAM_FILE))) : String(code_dir)
    started = utc_now()
    record = Dict{String,Any}(
        "convention" => CONVENTION, "id" => id, "key" => key,
        "name" => name === nothing ? nothing : String(name),
        "code" => code_block(cdir, code_name, code_version),
        "problem" => pr, "params" => Dict{String,Any}("key" => key),
        "host" => host_block(p),
        "lineage" => lineage === nothing ? Dict{String,Any}() : lineage,
        "status" => Dict{String,Any}("state" => "running", "started_at" => started, "last_event_seq" => 0))
    record = compact(record)
    write_json_atomic(joinpath(dir, "params.json"), params_document(p))   # before the first step
    write_json_atomic(joinpath(dir, "run.json"), record)
    write_outputs(dir, Dict{String,Any}[])
    run = Run(dir, id, key, record, p, 0, open_events(dir), ReentrantLock(), Dict{String,Any}[],
              false, on_checkpoint)
    emit!(run, "started")
    run
end

# ---- the verbs --------------------------------------------------------------------

progress!(run::Run; step, time = nothing, dt = nothing, fraction = nothing) =
    emit!(run, "progress", "step" => step, "time" => time, "dt" => dt, "fraction" => fraction)

function metric!(run::Run; step, values::AbstractDict)
    ok = Dict{String,Any}(string(k) => v for (k, v) in values if !(v isa Real && !isfinite(v)))
    bad = sort!([string(k) for (k, v) in values if v isa Real && !isfinite(v)])
    e = emit!(run, "metric", "step" => step, "values" => ok)
    isempty(bad) || warn!(run, "non-finite metric values omitted: " * join(bad, ", "); step,
                          data = Dict{String,Any}("non_finite" => bad))
    e
end

warn!(run::Run, msg::AbstractString; step = nothing, data = nothing) =
    emit!(run, "warning", "message" => String(msg), "step" => step, "data" => data)

note!(run::Run, msg::AbstractString = ""; data = nothing, step = nothing) =
    emit!(run, "note", "message" => isempty(msg) ? nothing : String(msg), "step" => step, "data" => data)

function relpath_in(run::Run, path::AbstractString)
    abs = isabspath(path) ? normpath(path) : normpath(joinpath(run.dir, path))
    isfile(abs) || throw(ArgumentError("$path does not exist (looked for $abs)"))
    root = normpath(abspath(run.dir)) * "/"
    rel = startswith(abs, root) ? abs[length(root)+1:end] : abs
    abs, rel
end

const OUTPUT_KINDS = ("snapshot", "checkpoint", "series", "spectrum", "image", "log", "other")
write_outputs(dir, outs) = write_json_atomic(joinpath(dir, "outputs.json"), Dict{String,Any}("outputs" => outs))

function index_output!(run::Run, path, kind, format, step, time)
    kind in OUTPUT_KINDS || throw(ArgumentError("output kind $(repr(kind)) must be one of $(OUTPUT_KINDS)"))
    abs, rel = relpath_in(run, path)
    entry = compact(Dict{String,Any}("path" => rel, "kind" => String(kind),
                             "format" => format === nothing ? (e = splitext(rel)[2]; isempty(e) ? nothing : e[2:end]) : String(format),
                             "sha256" => sha256_file(abs), "bytes" => filesize(abs)))
    step === nothing || (entry["step"] = step)
    time === nothing || (entry["time"] = time)
    lock(run.lock) do
        push!(run.outputs, entry)
        write_outputs(run.dir, run.outputs)
    end
    entry
end

function checkpoint!(run::Run, path::AbstractString; step, time = nothing, format = nothing, by = nothing)
    e = index_output!(run, path, "checkpoint", format, step, time)
    emit!(run, "checkpoint", "path" => e["path"], "sha256" => e["sha256"], "bytes" => e["bytes"],
          "step" => step, "by" => by)
end

function output!(run::Run, path::AbstractString; kind = "other", format = nothing, step = nothing,
                 time = nothing, schema = nothing)
    e = index_output!(run, path, kind, format, step, time)
    schema === nothing || (e["schema"] = String(schema); write_outputs(run.dir, run.outputs))
    emit!(run, "output", "path" => e["path"], "kind_of" => e["kind"], "format" => e["format"],
          "step" => step, "time" => time, "sha256" => e["sha256"], "bytes" => e["bytes"])
end

const EXIT_BANDS = (0, 100:199, 200:299, 300:399, 400:499)
valid_exit(code) = any(b -> code in b, EXIT_BANDS)

"""
    finish!(run; code, label, message=nothing, invalidates_reuse=nothing, state=nothing) -> Vector{String}

Append `finished`, report declared-but-unread parameters (a `warning` event and a log warning,
R-D2.5), and update only `run.json`'s `status`. `code` must lie in a convention band (0, 1xx..4xx).
State is derived: 0 done, 1xx cancelled, otherwise failed (override with `state=`).
Returns the unread parameter names.
"""
function finish!(run::Run; code::Integer, label::AbstractString, message = nothing,
                 invalidates_reuse = nothing, state = nothing)
    valid_exit(code) || throw(ArgumentError("exit code $code is outside the bands (0; 1xx; 2xx; 3xx; 4xx)"))
    run.finished && error("run $(run.id) is already finished")
    un = unread(run.params)
    if !isempty(un)
        msg = "declared parameters never read: " * join(un, ", ")
        @warn msg
        emit!(run, "warning", "message" => msg, "data" => Dict{String,Any}("unread" => un))
    end
    st = state !== nothing ? String(state) : code == 0 ? "done" : code in 100:199 ? "cancelled" : "failed"
    ex = Dict{String,Any}("code" => Int(code), "label" => String(label))
    message === nothing || (ex["message"] = String(message))
    invalidates_reuse === nothing || (ex["invalidates_reuse"] = Bool(invalidates_reuse))
    ev = emit!(run, "finished", "exit" => ex)
    lock(run.lock) do
        s = run.record["status"]
        s["state"] = st
        s["ended_at"] = ev["t"]
        s["exit"] = ex
        s["last_event_seq"] = run.seq
        write_json_atomic(joinpath(run.dir, "run.json"), run.record)
        run.finished = true
        close(run.io)
    end
    un
end

# ---- reading (readers tail; a trailing partial line is ignored, R-D6.6) ------------

"""
    read_events(dir_or_file; after=0) -> Vector{Dict}

Whole lines only: a trailing line without its newline is a write in progress and is ignored.
Throws if `seq` is not gap-free. Unknown kinds are kept (TR-D13).
"""
function read_events(path::AbstractString; after::Integer = 0)
    f = isdir(path) ? joinpath(path, "events.jsonl") : path
    isfile(f) || return Dict{String,Any}[]
    bytes = read(f)
    last = findlast(==(UInt8('\n')), bytes)
    last === nothing && return Dict{String,Any}[]
    out = Dict{String,Any}[]
    expect = 1
    for line in split(String(bytes[1:last]), '\n'; keepempty = false)
        e = parse_json(line)
        e["seq"] == expect || error("events.jsonl gap: expected seq $expect, found $(e["seq"])")
        expect += 1
        e["seq"] > after && push!(out, e)
    end
    out
end
