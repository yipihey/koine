# Koine as a research OS: skills, shipped defaults, the agent desk, packages

Omarchy (Arch + Hyprland, "beautiful, fun & agentic") ships what a system
needs to be safe for an agent to change: one self-describing command
surface, a hard line between what the system ships (`/usr/share/omarchy`)
and what the person owns (`~/.config`), a reset back to defaults, a skill
linked into every agent's skill directory, hand-offs from the desktop to
the agent (a crash notification hands the core dump over), and a package
path. Koine already has the harder half: every verb is typed, safety-
classed and declared once, and the pipeline has policy and consent. What it
lacks is the packaging. This plan adds five things and a package manager,
each as verbs over items, so people, agents and every frontend share them.

| Bet | Decision |
|---|---|
| An agent arriving at an app learns it in one read | S-D skills generated from the inventory |
| An agent may redesign a view without losing the shipped one | B-D shipped and yours |
| "Make me a view" is a front door, not a skill | G-D4 the make-a-surface request |
| What I'm pointing at is the agent's context; a failure hands itself over | G-D1..G-D3 the agent desk |
| What did we do, and how long did each step take | plan-koine-notebook |
| Surfaces, skills and requirements arrive as one reviewed unit | P-D packages |

The research OS these add up to, by layer:

| OS layer | Koine |
|---|---|
| Filesystem | `ItemStore`: every record an item, blobs beside it, attributed operations |
| Processes | verbs, jobs, `every`, workflows |
| Permissions | safety classes, policy, consent, declared secrets, caller identity |
| Window manager / IPC | `impress-layout` panes and selection channels |
| Display server | surfaces, rendered by SwiftUI, the TUI, the web and the phone |
| Shell | MCP, HTTP, the CLI and the generated forms over one inventory |
| `/usr/share` vs `~/.config` | B-D shipped and yours |
| Package manager | P-D packages |
| Man pages | S-D skills |
| Syslog and lab notebook | plan-koine-notebook |

## Skills (S-D)

### S-D1. A skill is generated from the inventory

`koine_agent::skill::render` writes a `SKILL.md` (YAML front matter with
`name` and `description`, then Markdown) from what the process links: the
app id, how to reach the host (MCP at `/mcp`; `POST /api/verb/<name>` with
the loopback token at `<workspace>/automation/loopback-<port>.token`), the
rules (consent: a write may answer `review-pending`, which is not a failure
to retry; try destructive verbs `tentatively` first; secrets are never
arguments; edit your copy of a shipped surface, never its baseline, and
`package-service_shipped-reset` puts it back), the adverbs, the record kinds, every verb by
service with its safety, secrets and `returns_kind`, how to make a surface
(validate, create, show), how to pick up agent-desk requests and how to
record a run. It is deterministic: two renders of one inventory are the same
bytes, so a checked-in copy can be diffed.

### S-D2. Three ways to read it

`agent-service_skill` answers it (an agent already connected over MCP),
`GET /api/skill` serves it as `text/markdown`, and the MCP host lists it as
the resource `koine://skill`.

### S-D3. `<host> skill install`

A host binary takes a subcommand before its flags: `skill print` writes the
skill to stdout; `skill install [--into <dir>]... [--force]` writes
`<dir>/<app-id>/SKILL.md` into each agent skill directory that exists
(`~/.claude/skills`, `~/.codex/skills`, `~/.agents/skills`), or into each
`--into`. A file koine did not write (no `generated-by: koine` line) is not
overwritten without `--force`. No server is started.

## Shipped and yours (B-D)

### B-D1. A shipped surface keeps its baseline beside it

Installing a shipped surface writes the surface (the person's copy, an
ordinary `impress/ui/surface@1.0.0` row, edited by the surface verbs like
any other) and a baseline item, `koine/shipped@1.0.0`: the surface id, the
package that shipped it, its name, version, the spec as shipped and its
digest. The person's copy is theirs; the baseline is the package's, written
only by the package verbs.

### B-D2. Upgrades are three-way

Installing a newer spec compares three things: the old baseline, the new
spec and the person's copy.

| Person's copy | Result |
|---|---|
| missing | created from the new spec |
| equal to the old baseline | replaced by the new spec |
| changed by the person | kept; the baseline records the new spec as `pending` and the status says `diverged` |

`package-service_shipped-take {id}` takes the pending spec (the person's copy is the undo),
`package-service_shipped-keep {id}` keeps theirs and clears the pending one. This replaces
the tour's rule (A-D8), which replaced a changed spec in place whether or
not the person had edited it; the tour itself is unchanged by this plan.

### B-D3. Reset and diff

`package-service_shipped-reset {id}` puts the shipped spec back and declares
`supports(reversibly)`: its inverse is `impress-surface-service_surface-
update` with the spec it replaced, so the undo is an ordinary verb call.
`package-service_shipped-diff {id}` answers the changes from baseline to copy as JSON
pointers (`added`, `removed`, `changed`, values cut at 200 characters).
`package-service_shipped-status` lists every baseline: `pristine`, `modified`, `diverged`
or `missing`.

### B-D4. The kit is a package

The surfaces every koine host ships (Notebook, Agent desk, Packages) are a
package built into the binary, `koine-kit`, installed at start through
B-D2. A person's edits to them survive an upgrade.

## The agent desk (G-D)

Koine runs no model (plan-koine-mobile M-D7). The desk is where a person
leaves work for whatever agent is connected and where the agent answers:
items of schema `koine/agent-request@1.0.0` with `kind` (`ask`,
`diagnose`, `make-surface`), `prompt`, `context`, `status` (`open`,
`claimed`, `answered`, `declined`), `claimed_by`, `answer` and `links`.

### G-D1. Pointing is context

`agent-service_context {app_id?, device?}` answers what the person is
pointing at: the focused pane (its view kind, query and the surface it
shows), what every channel carries (kind and ids, `impress-layout`'s
channels), and the verbs that act on the selected kind (those that return
it, and the verbs of the same service that take `id` or `ids`). `ask`
attaches it unless told not to.

### G-D2. Asking

`agent-service_ask {prompt, with_context?}` files an `ask` request.
`requests {status?}` lists them; `next {timeout_ms?}` waits (at most 30 s)
for an open one, so an agent's loop is one call; `claim`, `answer {id,
text, surface_id?}` and `decline {id, reason}` move one along. An answer
that names a surface can be shown: `show {id}` puts it in a new pane beside
the focused one, which is what the desk's "Show its surface" calls.

### G-D3. A failure hands itself over

`agent-service_diagnose {call_id, prompt?}` files a `diagnose` request whose
context is the call's record from the notebook (its steps, arguments as
recorded, outcome), the verb's descriptor and the run. The notebook's
Diagnose button calls it.

### G-D4. Make a surface

`agent-service_make-surface {description, name?}` files a `make-surface`
request whose context is what an agent needs to write the spec: the record
kinds, every verb with its description, a minimal example spec and the
verbs that validate, create and show it. The Agent desk surface puts the
field and starter prompts ("a table of …", "a plot of …", "a form for …")
in front of the person.

## Packages (P-D)

### P-D1. A package is a directory with a manifest

```json
{ "package": "1.0", "name": "paper-triage", "version": "0.2.0",
  "description": "…",
  "surfaces": [ { "name": "Paper triage", "file": "surfaces/triage.surface.json" } ],
  "skills":   [ { "name": "paper-triage", "file": "skills/SKILL.md" } ],
  "requires": { "verbs": ["scix-service_search"], "secrets": ["scix-api-token"] } }
```

Paths are relative, inside the directory, no `..`. The digest is SHA-256
over the manifest and every file it names, in a fixed order.

### P-D2. Review, then install exactly what was reviewed

`package-service_inspect {path}` answers the review: the digest, each surface
validated, every verb the surfaces call with its safety class, secrets and
whether it is linked, the required verbs and secrets that are missing, and
what installing would do to each surface (B-D2). `package-service_install {path,
digest}` refuses `digest-mismatch` unless the digest is the one inspected,
so nothing changes between review and install. Installing writes the
surfaces through B-D1, keeps the skills on the package item for `skill
install`, and records `koine/package@1.0.0` (`name`, `version`, `digest`,
`path`, `installed_at`, `surfaces`).

### P-D3. Removing keeps what is yours

`package-service_remove {name}` deletes the package's surfaces that are pristine and
keeps, without a baseline, those the person changed. `package-service_list` lists
what is installed.

### P-D4. Not yet

Signatures (a publisher key beside the digest), a registry to fetch from,
runtime providers a package starts (`koine-mcp-host::providers`), and
workflows and layout presets as package contents.

## Keyboard and palette

The impress apps' keymap (`impress-keymap`) is the place for "ask an agent
about the selection" and "make a surface" chords, as palette commands that
call `agent-service_ask` and open the Agent desk. They are not added here:
a binding with no implementation in impress-apps is a dead affordance
(plan-koine-affordances).

## Work packages

1. plan-koine-notebook.
2. `koine-agent`: the skill renderer, the desk, the Agent desk surface.
3. `koine-packages`: baselines, packages, the kit, the Packages surface.
4. `koine-host`: link both, install the kit, `GET /api/skill`, the MCP
   resource, `skill print|install`.

## What pins this

| Rule | Test |
|---|---|
| The skill names every linked verb and is the same bytes twice | `koine-agent` skill tests |
| `skill install` writes where agents look and keeps a foreign file | `koine-host` cli tests |
| The context answers the focused pane and the channels | `koine-agent` service tests |
| Ask, next, claim, answer move a request; diagnose carries the record | `koine-agent` service tests |
| Upgrade is three-way; reset restores and is reversible; diff names the change | `koine-packages` tests |
| Install refuses a changed directory; remove keeps a modified surface | `koine-packages` tests |
| The kit's surfaces validate and are installed at host start | `koine-host` `tests/host.rs` |

## Status

Landed 2026-10-15 on branch `research-os`: everything above except P-D4
and the chords. `koine-agent` (11 verbs), `koine-packages` (9) and
`koine-notebook` (10) are linked by every host through
`koine_host::ensure_linked`; `Config::kit` (default on) installs the kit at
start and `Config::app_version` stamps the call log and the skill. Each
crate's `tests/` pins its rules; `koine-host`'s `tests/host.rs`
(`the_kit_the_call_log_and_the_skill_are_served`,
`an_edited_kit_surface_survives_a_restart`) pins them over HTTP.
