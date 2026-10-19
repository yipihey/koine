# Declarations (R-D2.0). One @param line is the one place a parameter exists.

const MUTABILITY = ("never", "restart", "always")
const NAME_RE = r"^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+$"

struct ParamSpec
    name::String
    type::Any
    default::Any
    unit::Union{Nothing,String}
    group::String
    mutable::String
    doc::String
    env::Union{Nothing,String}
    advanced::Bool
    min::Any
    max::Any
    choices::Any
    requires::Union{Nothing,String}
end

struct MetricSpec
    name::String
    unit::String
    doc::String
end

struct OpenGroup
    name::String
    generator::Any
end

mutable struct Registry
    params::Dict{String,ParamSpec}
    order::Vector{String}
    metrics::Dict{String,MetricSpec}
    open_groups::Dict{String,OpenGroup}
end
Registry() = Registry(Dict{String,ParamSpec}(), String[], Dict{String,MetricSpec}(), Dict{String,OpenGroup}())

"The module-level registry the macros write into."
const REGISTRY = Registry()

function Base.empty!(r::Registry)
    empty!(r.params); empty!(r.order); empty!(r.metrics); empty!(r.open_groups)
    r
end

component_of(name::AbstractString) = String(first(split(name, '.')))

function register_param!(name::AbstractString, T, default; unit = nothing, group = nothing,
                         mutable = :never, doc = "", env = nothing, advanced = false,
                         min = nothing, max = nothing, choices = nothing, requires = nothing,
                         registry::Registry = REGISTRY)
    occursin(NAME_RE, name) || throw(ArgumentError(
        "parameter name '$name' must be component.name (e.g. sink.tau), never a flat global"))
    mut = String(mutable)
    mut in MUTABILITY || throw(ArgumentError(
        "parameter '$name': mutable must be one of :never, :restart, :always (got :$mut)"))
    d = coerce_value(T, default, name)
    spec = ParamSpec(String(name), T, d, unit === nothing ? nothing : String(unit),
                     group === nothing ? component_of(name) : String(group), mut, String(doc),
                     env === nothing ? nothing : String(env), Bool(advanced), min, max,
                     choices === nothing ? nothing : collect(choices),
                     requires === nothing ? nothing : String(requires))
    check_constraints(spec, d)
    haskey(registry.params, name) || push!(registry.order, String(name))
    registry.params[String(name)] = spec
    spec
end

function register_metric!(name::AbstractString, unit, doc; registry::Registry = REGISTRY)
    registry.metrics[String(name)] = MetricSpec(String(name), String(unit), String(doc))
end

function register_open_group!(name::AbstractString, generator; registry::Registry = REGISTRY)
    generator isa Base.Callable || throw(ArgumentError(
        "open group '$name' has no generator: an open group without one is a schema error"))
    registry.open_groups[String(name)] = OpenGroup(String(name), generator)
end

function dotted(ex)
    ex isa Symbol && return String(ex)
    ex isa QuoteNode && return String(ex.value)
    if ex isa Expr && ex.head == :. && length(ex.args) == 2
        return dotted(ex.args[1]) * "." * dotted(ex.args[2])
    end
    error("@param: expected a dotted name like sink.tau, got $ex")
end

function kwcollect(kws)
    out = Any[]
    for k in kws
        (k isa Expr && k.head == :(=) && k.args[1] isa Symbol) ||
            error("@param/@metric: expected keyword=value, got $k")
        push!(out, Expr(:kw, k.args[1], esc(k.args[2])))
    end
    out
end

"""
    @param module.name::T = default unit="…" group="…" mutable=:never doc="…" env="…"

Declare a parameter where it is read. Optional keywords: `unit`, `group`, `mutable`
(`:never` default, `:restart`, `:always`), `doc`, `env`, `advanced`, `min`, `max`,
`choices`, `requires="other.name=value"`.
"""
macro param(decl, kws...)
    (decl isa Expr && decl.head == :(=) && decl.args[1] isa Expr && decl.args[1].head == :(::)) ||
        error("@param: expected `component.name::Type = default`")
    lhs, default = decl.args
    name = dotted(lhs.args[1])
    T = lhs.args[2]
    ref = GlobalRef(@__MODULE__, :register_param!)
    Expr(:call, ref, Expr(:parameters, kwcollect(kws)...), name, esc(T), esc(default))
end

"""
    @open_group name generator

Declare a group whose parameters the code's own tables generate. `generator()` returns
`[(name, type, default, doc), …]` and is called at schema/params time.
"""
macro open_group(name, generator)
    ref = GlobalRef(@__MODULE__, :register_open_group!)
    Expr(:call, ref, dotted(name), esc(generator))
end

"""
    @metric name unit="…" doc="…"

Declare a metric the code emits through `metric!`; it appears under `x-run.metrics`.
"""
macro metric(name, kws...)
    ref = GlobalRef(@__MODULE__, :register_metric!)
    kw = Dict{Symbol,Any}()
    for k in kws
        (k isa Expr && k.head == :(=)) || error("@metric: expected keyword=value")
        kw[k.args[1]] = k.args[2]
    end
    Expr(:call, ref, String(name), esc(get(kw, :unit, "")), esc(get(kw, :doc, "")))
end

"All specs: declared ones in declaration order, then each open group's generated ones."
function all_specs(r::Registry)
    specs = ParamSpec[r.params[n] for n in r.order]
    seen = Set(r.order)
    for (g, og) in sort!(collect(r.open_groups); by = first)
        for ent in og.generator()
            n, T, d, doc = ent
            full = startswith(String(n), g * ".") ? String(n) : g * "." * String(n)
            full in seen && throw(ArgumentError("open group '$g' redeclares parameter '$full'"))
            push!(seen, full)
            spec = ParamSpec(full, T, coerce_value(T, d, full), nothing, g, "never", String(doc),
                             nothing, false, nothing, nothing, nothing, nothing)
            push!(specs, spec)
        end
    end
    specs
end

open_group_names(r::Registry) = Set(keys(r.open_groups))

json_type(T) = T <: Bool ? "boolean" : T <: Integer ? "integer" : T <: Real ? "number" :
               T <: AbstractString ? "string" : (T <: AbstractVector || T <: Tuple) ? "array" :
               T <: AbstractDict ? "object" : nothing

"""
    schema(; registry) -> Dict

The JSON Schema (draft 2020-12) of every declared parameter, with `x-run` extensions (R-D2.1).
"""
function schema(; registry::Registry = REGISTRY)
    props = Dict{String,Any}()
    for s in all_specs(registry)
        p = Dict{String,Any}("default" => s.default, "description" => s.doc)
        jt = json_type(s.type)
        jt === nothing || (p["type"] = jt)
        s.min === nothing || (p["minimum"] = s.min)
        s.max === nothing || (p["maximum"] = s.max)
        s.choices === nothing || (p["enum"] = s.choices)
        x = Dict{String,Any}("mutable" => s.mutable, "group" => s.group)
        s.unit === nothing || (x["unit"] = s.unit)
        s.env === nothing || (x["env"] = s.env)
        s.advanced && (x["advanced"] = true)
        s.requires === nothing || (x["requires"] = s.requires)
        p["x-run"] = x
        props[s.name] = p
    end
    metrics = [Dict{String,Any}("name" => m.name, "unit" => m.unit, "description" => m.doc)
               for m in sort!(collect(values(registry.metrics)); by = m -> m.name)]
    groups = Dict{String,Any}(g => Dict{String,Any}("open" => true) for g in keys(registry.open_groups))
    xrun = Dict{String,Any}("metrics" => metrics)
    isempty(groups) || (xrun["groups"] = groups)
    Dict{String,Any}("\$schema" => "https://json-schema.org/draft/2020-12/schema",
                     "type" => "object", "properties" => props,
                     "additionalProperties" => false, "x-run" => xrun)
end

schema_json(; registry::Registry = REGISTRY, indent::Int = 2) =
    json_string(schema(; registry); indent)

"`--schema` handling for a code's main: print the schema and exit when asked."
function maybe_print_schema(argv = ARGS; registry::Registry = REGISTRY, io::IO = stdout)
    "--schema" in argv || return false
    println(io, schema_json(; registry))
    true
end
