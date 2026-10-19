"""
    RunConvention

The Julia reference library of the run convention (`run@1`, koine docs/plan-run-convention.md).
Declare parameters where they are read, resolve them with sources, and keep a run directory
(`run.json`, `params.json`, `events.jsonl`, `outputs.json`, `control/`). File-based, no HTTP,
no scheduling, no knowledge of koine.
"""
module RunConvention

using Dates: Dates
using JSON3: JSON3
using SHA: SHA
using UUIDs: UUIDs

include("json.jl")
include("registry.jl")
include("params.jl")
include("run.jl")
include("control.jl")
include("resume.jl")

export @param, @open_group, @metric, params, schema, schema_json, maybe_print_schema, unread,
       start_run, resume, progress!, metric!, checkpoint!, output!, warn!, note!, finish!,
       poll_control!, read_events, run_key, canonical_json, Registry, REGISTRY,
       UnknownParameterError, ParameterValueError, ImmutableParameterError

end
