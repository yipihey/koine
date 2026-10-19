# Resolution: default -> params file -> environment -> command line (R-D2.5, R-D3).

struct UnknownParameterError <: Exception
    unknown::Vector{Tuple{String,String,String}}   # (name, source, suggestion-or-empty)
end
function Base.showerror(io::IO, e::UnknownParameterError)
    println(io, "unknown parameter", length(e.unknown) == 1 ? "" : "s",
            " (refusing the run; pass relaxed=true to downgrade to a warning):")
    for (n, src, sug) in e.unknown
        print(io, "  '", n, "' from ", src)
        isempty(sug) || print(io, "; did you mean '", sug, "'?")
        println(io)
    end
end

struct ParameterValueError <: Exception
    name::String
    msg::String
end
Base.showerror(io::IO, e::ParameterValueError) = print(io, "parameter '", e.name, "': ", e.msg)

struct ImmutableParameterError <: Exception
    name::String
    from::Any
    to::Any
    cls::String
end
Base.showerror(io::IO, e::ImmutableParameterError) = print(io, "parameter '", e.name,
    "' is mutable=:", e.cls, " and cannot change from ", repr(e.from), " to ", repr(e.to),
    " when resuming a checkpoint")

# ---- value coercion ---------------------------------------------------------------

function coerce_value(T, v, name)
    try
        v isa T && return v
        if T <: Bool
            v isa Bool && return v
        elseif T <: Integer
            v isa Real && isinteger(v) && return T(v)
        elseif T <: AbstractFloat
            v isa Real && !(v isa Bool) && return T(v)
            v isa AbstractString && lowercase(v) in ("nan", "inf", "-inf", "infinity", "-infinity") &&
                return T(parse(Float64, replace(lowercase(v), "infinity" => "inf")))
        elseif T <: AbstractString
            v isa AbstractString && return T(v)
        elseif T <: Symbol
            v isa AbstractString && return Symbol(v)
        elseif T === Any
            return v
        else
            return convert(T, v)
        end
    catch err
        err isa InterruptException && rethrow()
    end
    throw(ParameterValueError(String(name), "expected $T, got $(repr(v)) ($(typeof(v)))"))
end

function parse_text(T, s::AbstractString, name)
    s = String(s)
    if T <: AbstractString
        return coerce_value(T, s, name)
    elseif T <: Bool
        l = lowercase(strip(s))
        l in ("true", "1", "yes", "on") && return true
        l in ("false", "0", "no", "off") && return false
        throw(ParameterValueError(String(name), "expected a Bool, got '$s'"))
    elseif T <: Integer
        v = tryparse(Int64, strip(s))
        v === nothing && (f = tryparse(Float64, strip(s)); f !== nothing && isinteger(f) && (v = Int64(f)))
        v === nothing && throw(ParameterValueError(String(name), "expected an integer, got '$s'"))
        return coerce_value(T, v, name)
    elseif T <: AbstractFloat
        v = tryparse(Float64, strip(s))
        v === nothing && throw(ParameterValueError(String(name), "expected a number, got '$s'"))
        return coerce_value(T, v, name)
    elseif T <: Symbol
        return Symbol(s)
    else
        parsed = try
            parse_json(s)
        catch
            throw(ParameterValueError(String(name), "expected JSON for $T, got '$s'"))
        end
        return coerce_value(T, parsed, name)
    end
end

function check_constraints(s::ParamSpec, v)
    if s.min !== nothing && v isa Real && v < s.min
        throw(ParameterValueError(s.name, "$(repr(v)) is below minimum $(s.min)"))
    end
    if s.max !== nothing && v isa Real && v > s.max
        throw(ParameterValueError(s.name, "$(repr(v)) is above maximum $(s.max)"))
    end
    if s.choices !== nothing && !any(c -> isequal(c, v), s.choices)
        throw(ParameterValueError(s.name, "$(repr(v)) is not one of $(s.choices)"))
    end
    nothing
end

# ---- the resolved object ----------------------------------------------------------

"""
Resolved parameters. `p["sink.tau"]` and `p.sink.tau` read a value and mark it read;
`unread(p)` lists declared names the code never read.
"""
mutable struct Params
    specs::Dict{String,ParamSpec}
    order::Vector{String}
    values::Dict{String,Any}
    sources::Dict{String,String}
    read::Set{String}
    open::Set{String}
    lock::ReentrantLock
end

struct ParamNS
    p::Params
    prefix::String
end

function getparam(p::Params, name::AbstractString)
    v = lock(getfield(p, :lock)) do
        vals = getfield(p, :values)
        haskey(vals, name) || throw(KeyError(name))
        push!(getfield(p, :read), String(name))
        vals[name]
    end
    v
end
Base.getindex(p::Params, name::AbstractString) = getparam(p, name)
Base.haskey(p::Params, name::AbstractString) = haskey(getfield(p, :values), name)
Base.keys(p::Params) = copy(getfield(p, :order))

function Base.getproperty(p::Params, s::Symbol)
    s in fieldnames(Params) && return getfield(p, s)
    ns = ParamNS(p, String(s))
    isempty(namespace_keys(ns)) && throw(ArgumentError("no parameter component '$s'"))
    ns
end
Base.propertynames(p::Params) = Symbol.(unique(component_of.(getfield(p, :order))))

function namespace_keys(ns::ParamNS)
    pre = ns.prefix * "."
    [k for k in getfield(ns.p, :order) if startswith(k, pre)]
end
function Base.getproperty(ns::ParamNS, s::Symbol)
    s in (:p, :prefix) && return getfield(ns, s)
    full = getfield(ns, :prefix) * "." * String(s)
    p = getfield(ns, :p)
    haskey(getfield(p, :values), full) && return getparam(p, full)
    sub = ParamNS(p, full)
    isempty(namespace_keys(sub)) && throw(ArgumentError("no parameter '$full'"))
    sub
end
Base.propertynames(ns::ParamNS) = Symbol.(unique([split(k[length(ns.prefix)+2:end], '.')[1] for k in namespace_keys(ns)]))

"Declared parameters never read (open-group names excluded; a generated table is not hand-read)."
function unread(p::Params)
    lock(p.lock) do
        sort!([n for n in p.order if !(n in p.read) && !(component_of(n) in p.open)])
    end
end

"Plain Dict of name => value, without marking anything read."
values_of(p::Params) = Dict{String,Any}(k => v for (k, v) in p.values)
sources_of(p::Params) = copy(p.sources)

"The params.json document: every name, its value and its source (R-D3)."
params_document(p::Params) = Dict{String,Any}(n => Dict{String,Any}("value" => p.values[n],
                                              "source" => p.sources[n]) for n in p.order)

# ---- resolution -------------------------------------------------------------------

function edit_distance(a::String, b::String)
    m, n = length(a), length(b)
    d = collect(0:n)
    for i in 1:m
        prev = d[1]; d[1] = i
        for j in 1:n
            tmp = d[j+1]
            d[j+1] = min(d[j+1] + 1, d[j] + 1, prev + (a[i] == b[j] ? 0 : 1))
            prev = tmp
        end
    end
    d[n+1]
end

function suggest(name::String, known)
    best, bd = "", typemax(Int)
    for k in known
        dd = edit_distance(name, k)
        dd < bd && ((best, bd) = (k, dd))
    end
    bd <= max(2, length(name) ÷ 4) ? best : ""
end

"Read a parameters file: the params.json shape, or a plain {name: value} JSON object."
function read_params_file(path::AbstractString)
    doc = read_json(path)
    doc isa AbstractDict || throw(ArgumentError("$path: expected a JSON object"))
    out = Dict{String,Any}()
    for (k, v) in doc
        out[k] = (v isa AbstractDict && haskey(v, "value")) ? v["value"] : v
    end
    out
end

function parse_argv(argv)
    files = String[]; sets = Pair{String,String}[]; relaxed = false
    i = 1
    while i <= length(argv)
        a = String(argv[i])
        if a == "--params" && i < length(argv)
            push!(files, String(argv[i+1])); i += 1
        elseif startswith(a, "--params=")
            push!(files, a[length("--params=")+1:end])
        elseif a == "--relaxed"
            relaxed = true
        elseif startswith(a, "--") && occursin('=', a)
            k, v = split(a[3:end], '='; limit = 2)
            occursin('.', k) && push!(sets, String(k) => String(v))   # only component.name is ours
        end
        i += 1
    end
    files, sets, relaxed
end

const LAST_PARAMS = Ref{Union{Nothing,Params}}(nothing)

"""
    params(; argv=ARGS, file=nothing, env=ENV, relaxed=false, registry=REGISTRY) -> Params

Resolve default -> params file (`--params path` or `file=`) -> environment variable named by
`env=` -> `--name=value`, recording each value's source. An unknown name is an error
(`UnknownParameterError`) unless `relaxed=true` (or `--relaxed`), which warns instead.
"""
function params(; argv = ARGS, file = nothing, env = ENV, relaxed::Bool = false,
                registry::Registry = REGISTRY)
    specs_v = all_specs(registry)
    specs = Dict{String,ParamSpec}(s.name => s for s in specs_v)
    order = [s.name for s in specs_v]
    vals = Dict{String,Any}(s.name => s.default for s in specs_v)
    srcs = Dict{String,String}(s.name => "default" for s in specs_v)
    files, sets, relaxed_flag = parse_argv(argv)
    relaxed = relaxed || relaxed_flag
    unknown = Tuple{String,String,String}[]
    setval!(n, v, src) = (check_constraints(specs[n], v); vals[n] = v; srcs[n] = src)

    allfiles = String[]
    file === nothing || append!(allfiles, file isa AbstractString ? [String(file)] : String.(file))
    append!(allfiles, files)
    for f in allfiles
        src = "file:" * basename(f)
        for (k, v) in sort!(collect(read_params_file(f)); by = first)
            if haskey(specs, k)
                setval!(k, coerce_value(specs[k].type, v, k), src)
            else
                push!(unknown, (k, src, suggest(k, keys(specs))))
            end
        end
    end
    for n in order
        s = specs[n]
        if s.env !== nothing && haskey(env, s.env)
            setval!(n, parse_text(s.type, env[s.env], n), "env:" * s.env)
        end
    end
    for (k, v) in sets
        if haskey(specs, k)
            setval!(k, parse_text(specs[k].type, v, k), "cli")
        else
            push!(unknown, (k, "cli", suggest(k, keys(specs))))
        end
    end
    if !isempty(unknown)
        if relaxed
            for (n, src, sug) in unknown
                @warn "ignoring unknown parameter '$n' from $src (relaxed)" suggestion = sug
            end
        else
            throw(UnknownParameterError(unknown))
        end
    end
    check_requires(specs, order, vals, srcs)
    p = Params(specs, order, vals, srcs, Set{String}(), open_group_names(registry), ReentrantLock())
    LAST_PARAMS[] = p
    p
end

# R-D2.6: cross-parameter constraints, checked before anything is allocated.
function check_requires(specs, order, vals, srcs)
    for n in order
        s = specs[n]
        # A value sitting at its declared default requires nothing, whatever
        # its source: a resume reads the parent's file, and a parent that never
        # set `sink.tau` must not trip `requires` on the child (found by titan R3).
        (s.requires === nothing || srcs[n] == "default" || isequal(vals[n], s.default)) && continue
        for clause in split(s.requires, ',')
            o, want = occursin('=', clause) ? split(strip(clause), '='; limit = 2) : (strip(clause), nothing)
            haskey(vals, o) || throw(ParameterValueError(n, "requires unknown parameter '$o'"))
            ok = want === nothing ? (vals[o] != false && vals[o] != 0) :
                 isequal(vals[o], parse_text(specs[o].type, want, o))
            ok || throw(ParameterValueError(n,
                "is set (from $(srcs[n])) but requires $(strip(clause)); $o is $(repr(vals[o]))"))
        end
    end
end

"The key (R-D4.3): sha256 hex over the RFC 8785 canonical JSON of {problem, params-values}."
run_key(problem, values) = sha256_hex(canonical_json(Dict{String,Any}("problem" => problem, "params" => values)))
