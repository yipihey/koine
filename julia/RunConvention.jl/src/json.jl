# JSON in and out. Reading is JSON3; writing is ours because RFC 8785 (the key)
# needs control over number and key formatting, and run files want a stable
# key order (seq, t, kind first).

plain(x) = x
plain(x::JSON3.Object) = Dict{String,Any}(string(k) => plain(v) for (k, v) in pairs(x))
plain(x::JSON3.Array) = Any[plain(v) for v in x]

"Parse JSON text into plain Dict{String,Any} / Vector{Any} / scalars."
parse_json(s::AbstractString) = plain(JSON3.read(s))
read_json(path::AbstractString) = parse_json(read(path, String))

const PRIORITY_KEYS = ["seq", "t", "kind"]

function escape_json(io::IO, s::AbstractString)
    print(io, '"')
    for c in s
        if c == '"'
            print(io, "\\\"")
        elseif c == '\\'
            print(io, "\\\\")
        elseif c == '\b'
            print(io, "\\b")
        elseif c == '\f'
            print(io, "\\f")
        elseif c == '\n'
            print(io, "\\n")
        elseif c == '\r'
            print(io, "\\r")
        elseif c == '\t'
            print(io, "\\t")
        elseif c < ' '
            print(io, "\\u", string(UInt16(c); base = 16, pad = 4))
        else
            print(io, c)
        end
    end
    print(io, '"')
end

"ECMAScript Number::toString for a finite double (RFC 8785 section 3.2.2.3)."
function es_number(x::Real)
    x = Float64(x)
    isfinite(x) || throw(ArgumentError("canonical JSON cannot hold $x"))
    iszero(x) && return "0"
    neg = x < 0
    s = repr(abs(x))
    mant, ex = occursin('e', s) ? (split(s, 'e')...,) : (s, "0")
    e = parse(Int, ex)
    ip, fp = occursin('.', mant) ? split(mant, '.') : (mant, "")
    all = ip * fp
    z = length(all) - length(lstrip(==('0'), all))
    digits = rstrip(==('0'), lstrip(==('0'), all))
    isempty(digits) && (digits = "0")
    k = length(digits)
    n = length(ip) + e - z
    out = if k <= n <= 21
        digits * "0"^(n - k)
    elseif 0 < n <= 21
        digits[1:n] * "." * digits[n+1:end]
    elseif -6 < n <= 0
        "0." * "0"^(-n) * digits
    else
        ee = n - 1
        sg = ee < 0 ? "-" : "+"
        (k == 1 ? digits : digits[1:1] * "." * digits[2:end]) * "e" * sg * string(abs(ee))
    end
    neg ? "-" * out : out
end

utf16key(s::AbstractString) = transcode(UInt16, String(s))

function ordered_keys(d::AbstractDict, canonical::Bool)
    ks = String[string(k) for k in keys(d)]
    if canonical
        sort!(ks; by = utf16key)
    else
        sort!(ks; by = k -> (something(findfirst(==(k), PRIORITY_KEYS), 99), k))
    end
    ks
end

function emit_json(io::IO, x, canonical::Bool, indent::Int, level::Int)
    nl(l) = indent > 0 ? print(io, '\n', " "^(indent * l)) : nothing
    if x === nothing || x === missing
        print(io, "null")
    elseif x isa Bool
        print(io, x ? "true" : "false")
    elseif x isa Integer
        print(io, x)
    elseif x isa AbstractFloat
        if canonical
            print(io, es_number(x))
        elseif isnan(x)
            print(io, "\"NaN\"")
        elseif isinf(x)
            print(io, x > 0 ? "\"Infinity\"" : "\"-Infinity\"")
        else
            print(io, repr(Float64(x)))
        end
    elseif x isa AbstractString || x isa Symbol
        escape_json(io, string(x))
    elseif x isa NamedTuple
        emit_json(io, Dict{String,Any}(string(k) => v for (k, v) in pairs(x)), canonical, indent, level)
    elseif x isa AbstractDict
        ks = ordered_keys(x, canonical)
        lk = Dict(string(k) => k for k in keys(x))
        print(io, '{')
        for (i, k) in enumerate(ks)
            i > 1 && print(io, ',')
            nl(level + 1)
            escape_json(io, k)
            print(io, indent > 0 ? ": " : ":")
            emit_json(io, x[lk[k]], canonical, indent, level + 1)
        end
        isempty(ks) || nl(level)
        print(io, '}')
    elseif x isa AbstractVector || x isa Tuple
        print(io, '[')
        for (i, v) in enumerate(x)
            i > 1 && print(io, ',')
            nl(level + 1)
            emit_json(io, v, canonical, indent, level + 1)
        end
        isempty(x) || nl(level)
        print(io, ']')
    else
        throw(ArgumentError("cannot serialize $(typeof(x)) as JSON"))
    end
end

"Serialize to JSON. `canonical=true` is RFC 8785; `indent>0` pretty-prints."
function json_string(x; canonical::Bool = false, indent::Int = 0)
    io = IOBuffer()
    emit_json(io, x, canonical, indent, 0)
    String(take!(io))
end

"RFC 8785 canonical JSON of `x`."
canonical_json(x) = json_string(x; canonical = true)

sha256_hex(s::AbstractString) = bytes2hex(SHA.sha256(codeunits(s)))
sha256_file(path::AbstractString) = open(io -> bytes2hex(SHA.sha256(io)), path)

"Write `x` as pretty JSON atomically (tmp file + rename) so readers never see half a file."
function write_json_atomic(path::AbstractString, x)
    tmp = path * ".tmp." * string(getpid())
    write(tmp, json_string(x; indent = 2) * "\n")
    mv(tmp, path; force = true)
    path
end

utc_now() = Dates.format(Dates.now(Dates.UTC), "yyyy-mm-dd\\THH:MM:SS.sss\\Z")
