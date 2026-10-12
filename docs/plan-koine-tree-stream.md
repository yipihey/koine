# Koine tree and stream: hierarchy, output that arrives, graphs, clicks, colour

After the live wave (`plan-koine-live.md`), ten more popular crates were
held against koine. Five gaps remained, in the order of how many apps each
blocks: no tree node (files, archives, outlines, dependency trees), no
stream (sockets, logs, inference tokens), no graph node, no clicks on a
plot or an image, no colour in the code node. This plan closes them.

| Gap | Decision |
|---|---|
| A hierarchy has no node | T-D1 `tree` |
| A verb answers once; output that arrives has nowhere to go | T-D2 a stream is a job that emits |
| Edges cannot be drawn | T-D3 `graph` |
| A plot or an image cannot be pointed at | T-D4 `on_click` with coordinates |
| The code node names a language and shows no colour | T-D5 highlighted spans |

## Decisions

### T-D1. `tree`: rows with a parent

`tree{rows, id_field, parent_field, label_field, bind, on_select}`. `rows`
resolves to an array of objects or the `{rows, …}` envelope (plan-koine-live
L-D5); `id_field` (default `id`) and `parent_field` (default `parent`) say
how rows nest, a row whose parent is null or absent is a root;
`label_field` (default `label`, then `title`, then `name`) is what a line
shows. `bind` is a literal `state.…` path to the list of expanded ids; the
tree reads it and a disclosure click sends `change` with the new list,
exactly as a field writes its value. A row click sends `select` with
`[id]`. The resolver does the nesting: `RenderKind::Tree { nodes }` is the
visible outline already computed, each node with `id`, `label`, `depth`,
`has_children`, `expanded`, `row` (the row itself, for a renderer that
wants a second column), in the order drawn, and only the nodes whose
ancestors are expanded. A row that names a parent the rows do not hold is
a root. A cycle is cut at the first repeated id. The three renderers draw
the outline: the web as a nested list with a disclosure per branch and
`j`/`k` within the tree like the table, the TUI as indented lines with
`▸`/`▾`, Swift later.

### T-D2. A stream is a job that emits

No new verb shape. `impress_service_core::job::emit(line: Value)` is the
second channel beside `progress`: a verb running as a job (`in_background`
or `every`) appends one value to its task row. The runner keeps the last
`LINES_LIMIT` (1000) under `lines` on the `task@1.0.0` row and a running
`line_count`, batching writes every 100 ms, so a verb that emits a thousand
lines a second costs ten writes. Every write is a store event, so the feed
(plan-koine-live L-D2) wakes any surface whose source reads the task kind,
and a `log{lines: "{{source.job.lines}}"}` over a `query` source for that
job's row is the live view, with nothing new in the vocabulary.
`GET /api/job/<id>/lines?after=<n>` answers `{ok, lines, next, total}` for a
client that wants the stream itself; `cancel` ends it. A socket app is
`subscribe(url)` run `in_background`, emitting each message; a container's
logs are the same verb over its API; an inference verb emits tokens. The
demo service gains `tick(n, every_ms)` to prove it end to end.

### T-D3. `graph`: nodes, edges, a layout

`graph{nodes, edges, id_field, label_field, source_field, target_field,
on_select}`: `nodes` and `edges` resolve to rows (or envelopes); the
resolver builds a `koine_plot::graph::GraphSpec`, `koine-plot` lays it out
(a deterministic force-directed layout, seeded by the node ids, a fixed
number of iterations, in `koine_plot::graph::layout`) and draws it as SVG
(`render_graph_svg`: circles with labels, lines for edges, `data-id` on
every node group), and the renderers place the SVG as they place a plot;
`RenderKind::Graph { spec, svg }`. A click on a node sends `select` with
`[id]`: the web reads the `data-id` of the clicked element through the
DOM event's target; the TUI lists the nodes as lines and `Enter` selects.
A graph of more than 500 nodes is drawn without labels.

### T-D4. `on_click` on a plot and an image, with coordinates

`plot{…, on_click}` and `image{…, on_click}`. A click on a plot sends
`click` with `{"x": <data x>, "y": <data y>}`: `koine_plot::svg::frame(spec)`
exposes the plot area and the axis ranges the renderer used, so the page
maps the click's element coordinates back through the same arithmetic (log
axes included) before it sends the event. A click on an image sends
`{"x": <fraction 0..1>, "y": <fraction 0..1>}` of the element's displayed
size, which is the only size a page knows for certain, so a verb that
wants pixels multiplies by the image's own dimensions. The reducer runs
`on_click` with `{{event.value.x}}` and `{{event.value.y}}`. The TUI has no
pointer: `Enter` on a plot or image sends `click` with `{"x": null, "y":
null}`, so a handler can tell. A drag rectangle is not in this plan.

### T-D5. Highlighted spans in the code node

`impress-surface` gains a `highlight` feature over `syntect` (the pure-Rust
regex engine, no onig). With it, the resolver fills
`RenderKind::Code.spans`: a list of `(class, text)` per line, the class a
short name of the syntect scope's first two parts (`keyword`, `string`,
`comment`, `type`, `function`, `variable`, `constant`, `punctuation`), so the
sheet colours a dozen classes and no more. Without the feature `spans` is
empty and the node reads as it did. `diff` keeps its line classes. The
host enables the feature; the browser bundle does not build syntect, since
the page receives the spans in the tree.

## Work packages

| # | Package | Where |
|---|---|---|
| T1 | `tree` (T-D1): spec, validate, resolve, the three renderers, goldens | impress-surface, koine-dioxus, koine-tui, koine-web, koine goldens |
| T2 | the stream (T-D2): `emit`, the runner's lines, the lines route, the demo verb, e2e | impress-service-core (job.rs), koine-host (runner, routes), surface-demo-service, e2e |
| T3a | graph layout and SVG, the plot frame (T-D3, T-D4 in `koine-plot`) | koine-plot |
| T3b | `graph` node, `on_click` on plot and image, highlighted spans, the renderers (T-D3, T-D4, T-D5) | impress-surface, koine-dioxus, koine-tui, koine-web, after T1 |
| T4 | wiring, docs, gate, rev bump | orchestrator |

## Status

Filled in as the packages land.
