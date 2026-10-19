using Test
using RunConvention
using RunConvention: canonical_json, es_number, read_json, parse_json, sha256_hex, sha256_file, values_of

function declare!()
    empty!(REGISTRY)
    @param sink.tau::Float64 = 0.0 unit="s" group="sink" mutable=:never env="QS_SINK_TAU" doc="inner sink relaxation time"
    @param grid.ncell::Int = 400 mutable=:never min=1 doc="cells"
    @param io.every::Int = 100 mutable=:always doc="output cadence"
    @param run.zend::Float64 = 1.0 mutable=:restart unit="Myr" doc="end time"
    @param run.label::String = "a" mutable=:restart choices=["a", "b"]
    @param run.tags::Vector{Float64} = [1.0, 2.0] mutable=:never
    @metric mach unit="" doc="shock Mach number"
end

mkrun(dir; argv = String[], kw...) = start_run(dir; params = params(; argv, env = Dict{String,String}()),
                                               problem = (name = "sod", version = "2", params = Dict("n" => 3)),
                                               code_dir = dir, kw...)
touch_ck(dir, name, txt = "ck") = (p = joinpath(dir, name); write(p, txt); p)

@testset "RunConvention" begin
    @testset "canonical JSON (RFC 8785)" begin
        @test es_number(1e21) == "1e+21"
        @test es_number(1e-7) == "1e-7"
        @test es_number(0.000001) == "0.000001"
        @test es_number(333333333.33333329) == "333333333.3333333"
        @test es_number(3.0) == "3"
        @test es_number(-0.0) == "0"
        @test es_number(1.5e300) == "1.5e+300"
        @test es_number(123456789012345680000.0) == "123456789012345680000"
        @test canonical_json(Dict("b" => 1, "a" => [true, nothing, "x\ty"], "€" => 1)) ==
              "{\"a\":[true,null,\"x\\ty\"],\"b\":1,\"€\":1}"
        @test canonical_json(NaN) == "\"NaN\"" && canonical_json(Inf) == "\"Infinity\""   # R-D4.3
    end

    @testset "declaration, schema" begin
        declare!()
        @test_throws ArgumentError RunConvention.register_param!("flat", Int, 1)
        @test_throws ArgumentError RunConvention.register_param!("a.b", Int, 1; mutable = :sometimes)
        @open_group titan () -> [("ql1", Float64, 1e-4, "ql coefficient"), ("titan.cq2", Int, 1, "cq")]
        s = schema()
        pr = s["properties"]
        @test s["\$schema"] == "https://json-schema.org/draft/2020-12/schema"
        @test pr["sink.tau"]["type"] == "number"
        @test pr["sink.tau"]["x-run"] == Dict("unit" => "s", "group" => "sink", "mutable" => "never", "env" => "QS_SINK_TAU")
        @test pr["grid.ncell"]["type"] == "integer" && pr["grid.ncell"]["minimum"] == 1
        @test pr["io.every"]["x-run"]["mutable"] == "always"
        @test pr["run.label"]["enum"] == ["a", "b"]
        @test pr["run.tags"]["type"] == "array"
        @test pr["titan.ql1"]["x-run"]["group"] == "titan" && pr["titan.cq2"]["type"] == "integer"
        @test s["x-run"]["groups"]["titan"]["open"] == true
        @test s["x-run"]["metrics"] == [Dict("name" => "mach", "unit" => "", "description" => "shock Mach number")]
        @test s["additionalProperties"] == false
        @test occursin("\"sink.tau\"", schema_json())
        @test_throws ArgumentError RunConvention.register_open_group!("bad", nothing)
    end

    @testset "resolution order and sources" begin
        declare!()
        d = mktempdir()
        write(joinpath(d, "in.json"), """{"grid.ncell": {"value": 800, "source": "x"}, "sink.tau": 5.0}""")
        p = params(; argv = ["--params", joinpath(d, "in.json"), "--grid.ncell=1600", "other", "--threads=4"],
                   env = Dict("QS_SINK_TAU" => "3e6"))
        @test p["sink.tau"] == 3.0e6 && p.sources["sink.tau"] == "env:QS_SINK_TAU"
        @test p.grid.ncell == 1600 && p.sources["grid.ncell"] == "cli"
        @test p.io.every == 100 && p.sources["io.every"] == "default"
        @test p.run.tags == [1.0, 2.0]
        p2 = params(; argv = ["--params=" * joinpath(d, "in.json")], env = Dict{String,String}())
        @test p2["grid.ncell"] == 800 && p2.sources["grid.ncell"] == "file:in.json"
        @test p2["sink.tau"] == 5.0
        p3 = params(; argv = ["--run.tags=[3,4]", "--run.label=b"], env = Dict{String,String}())
        @test p3.run.tags == [3.0, 4.0] && p3.run.label == "b"
        @test_throws RunConvention.ParameterValueError params(; argv = ["--run.label=zzz"], env = Dict{String,String}())
        @test_throws RunConvention.ParameterValueError params(; argv = ["--grid.ncell=0"], env = Dict{String,String}())
        @test_throws RunConvention.ParameterValueError params(; argv = ["--grid.ncell=1.5"], env = Dict{String,String}())
    end

    @testset "unknown is an error; relaxed warns" begin
        declare!()
        e = try params(; argv = ["--sink.taau=1"], env = Dict{String,String}()); nothing catch err; err end
        @test e isa UnknownParameterError
        @test occursin("sink.taau", sprint(showerror, e)) && occursin("did you mean 'sink.tau'", sprint(showerror, e))
        d = mktempdir(); write(joinpath(d, "f.json"), """{"nope.x": 1}""")
        @test_throws UnknownParameterError params(; argv = ["--params", joinpath(d, "f.json")], env = Dict{String,String}())
        @test_logs (:warn, r"ignoring unknown parameter 'nope.x'") params(; argv = ["--params", joinpath(d, "f.json")],
                   relaxed = true, env = Dict{String,String}())
        @test_logs (:warn, r"ignoring unknown") params(; argv = ["--zz.y=1", "--relaxed"], env = Dict{String,String}())
        # a flag with no component.name shape is the code's own, not ours
        @test params(; argv = ["--threads=4"], env = Dict{String,String}()) isa RunConvention.Params
    end

    @testset "requires (R-D2.6)" begin
        empty!(REGISTRY)
        @param a.gasrow::Int = 0
        @param a.tau::Float64 = 0.0 requires="a.gasrow=1"
        @test params(; argv = String[], env = Dict{String,String}())["a.tau"] == 0.0
        @test_throws ParameterValueError params(; argv = ["--a.tau=1"], env = Dict{String,String}())
        @test params(; argv = ["--a.tau=1", "--a.gasrow=1"], env = Dict{String,String}())["a.tau"] == 1.0
        # A file (a resume's parent) carrying the default value is not "set".
        f = joinpath(mktempdir(), "params.json")
        write(f, """{"a.tau": 0.0, "a.gasrow": 0}""")
        @test params(; argv = String[], env = Dict{String,String}(), file = f)["a.tau"] == 0.0
        write(f, """{"a.tau": 1.0, "a.gasrow": 0}""")
        @test_throws ParameterValueError params(; argv = String[], env = Dict{String,String}(), file = f)
    end

    @testset "run directory, events, finish" begin
        declare!()
        d = joinpath(mktempdir(), "r1")
        run = mkrun(d; argv = ["--sink.tau=2.5"], name = "first")
        rj = read_json(joinpath(d, "run.json"))
        @test rj["convention"] == "run@1" && rj["status"]["state"] == "running"
        @test length(rj["id"]) == 36 && rj["name"] == "first" && rj["key"] == rj["params"]["key"]
        @test rj["code"]["julia"] == string(VERSION) && rj["code"]["git_available"] == false && !haskey(rj["code"], "commit") && !haskey(rj["code"], "dirty")
        @test !any(v -> v === nothing, values(rj["host"])) && !any(v -> v === nothing, values(rj["code"]))
        @test haskey(rj["host"], "hostname") && rj["problem"]["name"] == "sod"
        pj = read_json(joinpath(d, "params.json"))
        @test pj["sink.tau"] == Dict("value" => 2.5, "source" => "cli")
        @test isdir(joinpath(d, "control"))
        @test_throws ArgumentError mkrun(d)   # no clobbering a run

        p = run.params
        p["sink.tau"]; p["grid.ncell"]; p["io.every"]; p["run.zend"]; p["run.label"]
        progress!(run; step = 100, time = 3.1e5, dt = 12.0, fraction = 0.03)
        progress!(run; step = 200, time = 6.2e5, dt = 12.0)
        metric!(run; step = 100, values = Dict("mach" => 1.42, "bad" => NaN))
        ck = touch_ck(d, "ckpt_000100.jls", "state")
        checkpoint!(run, ck; step = 100)
        snap = touch_ck(d, "snap.h5", "abcdef")
        output!(run, snap; kind = "snapshot", format = "hdf5", step = 100, time = 3.1e5)
        warn!(run, "dt below floor"; step = 211)
        @test_throws ArgumentError output!(run, joinpath(d, "missing.h5"); kind = "snapshot")
        un = finish!(run; code = 0, label = "ok")
        @test un == ["run.tags"]
        ev = read_events(d)
        @test [e["seq"] for e in ev] == collect(1:length(ev))
        @test [e["kind"] for e in ev] == ["started", "progress", "progress", "metric", "warning", "checkpoint", "output",
                                          "warning", "warning", "finished"]
        @test all(e -> occursin(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$", e["t"]), ev)
        @test !haskey(ev[3], "fraction") && ev[2]["fraction"] == 0.03
        @test ev[5]["data"]["non_finite"] == ["bad"]
        @test ev[4]["values"] == Dict("mach" => 1.42)
        @test ev[6]["sha256"] == bytes2hex(RunConvention.SHA.sha256("state")) && ev[6]["path"] == "ckpt_000100.jls"
        @test ev[7]["kind_of"] == "snapshot" && ev[7]["bytes"] == 6
        @test ev[9]["data"]["unread"] == ["run.tags"]
        @test ev[end]["exit"] == Dict("code" => 0, "label" => "ok")
        outs = read_json(joinpath(d, "outputs.json"))["outputs"]
        @test [o["kind"] for o in outs] == ["checkpoint", "snapshot"]
        @test outs[2]["sha256"] == sha256_file(snap) && outs[2]["bytes"] == 6 && outs[2]["format"] == "hdf5"
        rj2 = read_json(joinpath(d, "run.json"))
        st = rj2["status"]
        @test st["state"] == "done" && st["last_event_seq"] == 10 && st["exit"]["label"] == "ok" && st["ended_at"] == ev[end]["t"]
        # finish! touched only the status block
        delete!(rj, "status"); delete!(rj2, "status")
        @test rj == rj2
        @test_throws ErrorException progress!(run; step = 1, time = 0.0, dt = 0.0)
        @test_throws ErrorException finish!(run; code = 0, label = "ok")

        r2 = mkrun(joinpath(mktempdir(), "r2"))
        @test_throws ArgumentError finish!(r2; code = 5, label = "x")
        @test_throws ArgumentError finish!(r2; code = 500, label = "x")
        finish!(r2; code = 320, label = "nan-in-state", message = "boom", invalidates_reuse = true)
        s2 = read_json(joinpath(r2.dir, "run.json"))["status"]
        @test s2["state"] == "failed" && s2["exit"]["message"] == "boom" && s2["exit"]["invalidates_reuse"] == true
        r3 = mkrun(joinpath(mktempdir(), "r3")); finish!(r3; code = 110, label = "walltime")
        @test read_json(joinpath(r3.dir, "run.json"))["status"]["state"] == "cancelled"
    end

    @testset "git provenance" begin
        declare!()
        if Sys.which("git") !== nothing
            repo = mktempdir()
            g(args...) = run(pipeline(`git -C $repo -c user.email=t@t -c user.name=t $(collect(args))`; stdout = devnull))
            g("init", "-q"); write(joinpath(repo, "f.jl"), "1"); g("add", "."); g("commit", "-q", "-m", "x")
            head = strip(read(`git -C $repo rev-parse HEAD`, String))
            r = start_run(joinpath(mktempdir(), "g"); params = params(; argv = String[], env = Dict{String,String}()), code_dir = repo)
            c = read_json(joinpath(r.dir, "run.json"))["code"]
            @test c["commit"] == head && c["dirty"] == false && !haskey(c, "diff_sha256") && c["git_available"] == true
            write(joinpath(repo, "f.jl"), "2")
            r = start_run(joinpath(mktempdir(), "g2"); params = params(; argv = String[], env = Dict{String,String}()), code_dir = repo)
            c = read_json(joinpath(r.dir, "run.json"))["code"]
            @test c["dirty"] == true && c["diff_sha256"] isa String && length(c["diff_sha256"]) == 64
        end
        # Manifest hash is recorded when the active project has one
        mp = joinpath(dirname(Base.active_project()), "Manifest.toml")
        r = mkrun(joinpath(mktempdir(), "m"))
        mh = get(read_json(joinpath(r.dir, "run.json"))["code"], "lock_sha256", nothing)
        @test mh === (isfile(mp) ? sha256_file(mp) : nothing) || mh isa String
    end

    @testset "non-finite floats are strings, in the key too (R-D4.3)" begin
        empty!(REGISTRY)
        @param lim.max_dt::Float64 = Inf
        @param lim.gap::Float64 = 0.5
        vals = Dict("lim.max_dt" => Inf, "lim.gap" => NaN)
        @test canonical_json(vals) == """{"lim.gap":"NaN","lim.max_dt":"Infinity"}"""
        @test canonical_json(Dict("x" => -Inf)) == """{"x":"-Infinity"}"""
        f = joinpath(mktempdir(), "p.json")
        write(f, """{"lim.max_dt": "Infinity", "lim.gap": "NaN"}""")
        p = params(; argv = String[], env = Dict{String,String}(), file = f)
        @test p["lim.max_dt"] === Inf && isnan(p["lim.gap"])
        @test params(; argv = ["--lim.max_dt=Inf"], env = Dict{String,String}())["lim.max_dt"] === Inf
    end

    @testset "params.json round-trip reproduces the key" begin
        declare!()
        d = joinpath(mktempdir(), "a")
        run = mkrun(d; argv = ["--sink.tau=3e6", "--run.tags=[0.1,2e-9]", "--grid.ncell=64"])
        k1 = run.key
        finish!(run; code = 0, label = "ok")
        # key == sha256(canonical({problem, params})): the self-check any tool can run
        rj = read_json(joinpath(d, "run.json")); pj = read_json(joinpath(d, "params.json"))
        vals = Dict(k => v["value"] for (k, v) in pj)
        @test sha256_hex(canonical_json(Dict("problem" => rj["problem"], "params" => vals))) == rj["key"] == k1
        # feed it back
        p2 = params(; argv = ["--params", joinpath(d, "params.json")], env = Dict{String,String}())
        r2 = start_run(joinpath(mktempdir(), "b"); params = p2, problem = (name = "sod", version = "2", params = Dict("n" => 3)), code_dir = d)
        @test r2.key == k1
        @test all(((k, v),) -> startswith(v, "file:"), p2.sources)
        # a different value changes the key
        p3 = params(; argv = ["--params", joinpath(d, "params.json"), "--grid.ncell=65"], env = Dict{String,String}())
        @test start_run(joinpath(mktempdir(), "c"); params = p3, problem = (name = "sod", version = "2", params = Dict("n" => 3)), code_dir = d).key != k1
        # a different problem changes the key
        @test start_run(joinpath(mktempdir(), "e"); params = p2, problem = (name = "sod", version = "3", params = Dict("n" => 3)), code_dir = d).key != k1
    end

    @testset "mutability classes in poll_control!" begin
        declare!()
        d = joinpath(mktempdir(), "c")
        cks = Ref(0)
        run = mkrun(d; on_checkpoint = r -> (cks[] += 1; checkpoint!(r, touch_ck(d, "c$(cks[]).bin"); step = 5)))
        ctl(id, obj) = write(joinpath(d, "control", id * ".json"), RunConvention.json_string(obj))
        ctl("a1", Dict("request" => "set", "param" => "io.every", "value" => 10))
        ctl("a2", Dict("request" => "set", "param" => "run.zend", "value" => 2.0))     # restart: refused
        ctl("a3", Dict("request" => "set", "param" => "sink.tau", "value" => 1.0))     # never: refused
        ctl("a4", Dict("request" => "set", "param" => "no.such", "value" => 1))
        ctl("a5", Dict("request" => "set", "param" => "io.every", "value" => "oops"))
        ctl("a6", Dict("request" => "checkpoint"))
        ctl("a7", Dict("request" => "frobnicate"))
        @test poll_control!(run; step = 5) == :continue
        @test run.params["io.every"] == 10 && run.params.sources["io.every"] == "control:a1"
        @test run.params["run.zend"] == 1.0 && run.params["sink.tau"] == 0.0
        @test sort(readdir(joinpath(d, "control", "done"))) == ["a1.json", "a6.json"]
        @test sort(readdir(joinpath(d, "control", "refused"))) == ["a2.json", "a3.json", "a4.json", "a5.json", "a7.json"]
        @test occursin("not :always", read_json(joinpath(d, "control", "refused", "a2.json"))["reason"])
        @test occursin("mutable=:never", read_json(joinpath(d, "control", "refused", "a3.json"))["reason"]) ||
              occursin("not :always", read_json(joinpath(d, "control", "refused", "a3.json"))["reason"])
        @test isempty(filter(f -> endswith(f, ".json"), readdir(joinpath(d, "control"))))
        ev = read_events(d)
        ch = filter(e -> e["kind"] == "changed", ev)
        @test length(ch) == 1 && ch[1]["param"] == "io.every" && ch[1]["from"] == 100 && ch[1]["to"] == 10 &&
              ch[1]["by"] == "control:a1" && ch[1]["step"] == 5
        @test count(e -> e["kind"] == "checkpoint", ev) == 1 && cks[] == 1
        # params.json is never rewritten by steering
        @test read_json(joinpath(d, "params.json"))["io.every"]["value"] == 100
        # stop
        ctl("b1", Dict("request" => "stop", "after" => "checkpoint"))
        @test poll_control!(run) == :stop && cks[] == 2
        # pause blocks until continue
        ctl("p1", Dict("request" => "pause"))
        t = @async (sleep(0.3); ctl("p2", Dict("request" => "resume")))
        t0 = time(); @test poll_control!(run; pause_poll = 0.05) == :continue; @test time() - t0 >= 0.25
        wait(t)
        # pause that is never released times out
        ctl("p3", Dict("request" => "pause"))
        @test poll_control!(run; pause_poll = 0.02, pause_timeout = 0.1) == :continue
        # a half-written request is left alone
        write(joinpath(d, "control", "half.json"), "{\"request\": \"st")
        @test poll_control!(run) == :continue && isfile(joinpath(d, "control", "half.json"))
        # no checkpoint handler -> refused
        r2 = mkrun(joinpath(mktempdir(), "n"))
        write(joinpath(r2.dir, "control", "k.json"), """{"request":"checkpoint"}""")
        poll_control!(r2)
        @test occursin("no checkpoint handler", read_json(joinpath(r2.dir, "control", "refused", "k.json"))["reason"])
    end

    @testset "concurrent reader sees whole lines" begin
        declare!()
        d = joinpath(mktempdir(), "t")
        run = mkrun(d)
        n = 1500
        done = Threads.Atomic{Bool}(false)
        bad = Threads.Atomic{Int}(0); reads = Threads.Atomic{Int}(0); maxseen = Ref(0)
        reader = Threads.@spawn begin
            f = joinpath(d, "events.jsonl")
            while !done[]
                bytes = read(f)
                last = findlast(==(UInt8('\n')), bytes)
                last === nothing && continue
                lines = split(String(bytes[1:last]), '\n'; keepempty = false)
                for (i, l) in enumerate(lines)
                    e = try parse_json(l) catch; nothing end
                    (e === nothing || e["seq"] != i) && Threads.atomic_add!(bad, 1)
                end
                maxseen[] = max(maxseen[], length(lines))
                Threads.atomic_add!(reads, 1)
            end
        end
        for i in 1:n
            metric!(run; step = i, values = Dict("x" => i * 0.5, "pad" => "p"^50))
        end
        done[] = true; wait(reader)
        @test bad[] == 0 && reads[] > 0
        @test length(read_events(d)) == n + 1
        # a trailing partial line is ignored by the reader library
        open(joinpath(d, "events.jsonl"), "a") do io; write(io, "{\"seq\": $(n + 2), \"t\": \"2026"); end
        @test length(read_events(d)) == n + 1
        @test length(read_events(d; after = n)) == 1
        # a gap is reported
        write(joinpath(d, "events.jsonl"), "{\"seq\":1,\"kind\":\"started\"}\n{\"seq\":3,\"kind\":\"note\"}\n")
        @test_throws ErrorException read_events(d)
        # unknown kinds are kept
        write(joinpath(d, "events.jsonl"), "{\"seq\":1,\"kind\":\"mystery\",\"x\":1}\n")
        @test read_events(d)[1]["kind"] == "mystery"
    end

    @testset "resume: lineage and classes" begin
        declare!()
        pd = joinpath(mktempdir(), "parent")
        run = mkrun(pd; argv = ["--grid.ncell=64"], name = "p")
        ck = touch_ck(pd, "ckpt_000100.jls", "state-100")
        checkpoint!(run, ck; step = 100)
        finish!(run; code = 110, label = "walltime", state = "suspended")
        @test read_json(joinpath(pd, "run.json"))["status"]["state"] == "suspended"

        cd_ = joinpath(mktempdir(), "child")
        child = resume(cd_; from = ck, argv = ["--run.zend=5.0", "--io.every=7"], env = Dict{String,String}())
        cj = read_json(joinpath(cd_, "run.json"))
        @test cj["lineage"]["parent"] == Dict("run" => run.id, "relation" => "resume",
                  "checkpoint" => Dict("path" => "ckpt_000100.jls", "sha256" => sha256_file(ck)))
        @test !haskey(cj["lineage"], "sweep")
        @test cj["status"]["state"] == "running" && cj["name"] == "p" && cj["problem"] == read_json(joinpath(pd, "run.json"))["problem"]
        @test cj["id"] != run.id && cj["key"] != run.key
        @test child.params["grid.ncell"] == 64          # the parent's value is carried
        @test child.params["run.zend"] == 5.0
        ev = read_events(cd_)
        ch = filter(e -> e["kind"] == "changed", ev)
        @test sort([e["param"] for e in ch]) == ["io.every", "run.zend"]
        @test all(e -> e["by"] == "restart" && e["step"] == 100, ch)
        @test ev[1]["kind"] == "started"
        pj = read_json(joinpath(cd_, "params.json"))
        @test pj["run.zend"]["value"] == 5.0 && pj["grid.ncell"]["value"] == 64
        finish!(child; code = 0, label = "ok")

        # a changed `never` parameter is refused, and no run directory is created
        bad = joinpath(mktempdir(), "bad")
        e = try resume(bad; from = ck, argv = ["--grid.ncell=128"], env = Dict{String,String}()); nothing catch err; err end
        @test e isa ImmutableParameterError && e.name == "grid.ncell" && e.from == 64 && e.to == 128
        @test occursin("mutable=:never", sprint(showerror, e)) || occursin("mutable=:never", sprint(showerror, e))
        @test !isdir(bad)
        # the same value is not a change
        ok = resume(joinpath(mktempdir(), "same"); from = ck, argv = ["--grid.ncell=64"], env = Dict{String,String}())
        @test ok.key == run.key
        @test_throws ArgumentError resume(joinpath(mktempdir(), "x"); from = joinpath(pd, "nope.jls"))
        orphan = touch_ck(mktempdir(), "o.jls")
        @test_throws ArgumentError resume(joinpath(mktempdir(), "y"); from = orphan)
    end

    @testset "run-check conformance (R1 checker)" begin
        bin = get(ENV, "RUN_CHECK", "/Users/tabel/Projects/koine/target/debug/run-check")
        if !isfile(bin)
            @info "run-check not found at $bin; skipping (set RUN_CHECK to its path)"
        else
            check(d) = (out = IOBuffer();
                        pr = run(pipeline(ignorestatus(`$bin $d`); stdout = out, stderr = out));
                        (pr.exitcode, String(take!(out))))
            declare!()
            d = joinpath(mktempdir(), "full")
            run1 = nothing
            run1 = mkrun(d; argv = ["--sink.tau=2.5"], name = "conformance",
                         on_checkpoint = r -> checkpoint!(r, touch_ck(d, "ck_$(r.seq).jls", "s$(r.seq)"); step = 7))
            p = run1.params
            foreach(n -> p[n], keys(p))
            progress!(run1; step = 1, time = 0.5, dt = 0.5, fraction = 0.1)
            metric!(run1; step = 1, values = Dict("mach" => 1.4))
            write(joinpath(d, "control", "c1.json"), """{"request":"checkpoint"}""")
            write(joinpath(d, "control", "s1.json"), """{"request":"set","param":"io.every","value":5}""")
            write(joinpath(d, "control", "s2.json"), """{"request":"set","param":"sink.tau","value":1.0}""")
            @test poll_control!(run1; step = 7) == :continue
            output!(run1, touch_ck(d, "snap.h5", "x"); kind = "snapshot", format = "hdf5", step = 7, time = 1.0)
            warn!(run1, "dt low"; step = 7)
            finish!(run1; code = 0, label = "ok")
            code, out = check(d)
            @info "run-check parent" code out
            @test code == 0
            ck = joinpath(d, only(filter(e -> e["kind"] == "checkpoint", read_events(d)))["path"])
            child = resume(joinpath(mktempdir(), "child"); from = ck, argv = ["--run.zend=3.0"], env = Dict{String,String}())
            progress!(child; step = 8, time = 1.5, dt = 0.5)
            finish!(child; code = 0, label = "ok")
            code, out = check(child.dir)
            @info "run-check child" code out
            @test code == 0
        end
    end
end
