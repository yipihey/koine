//! A graph: nodes, edges, a deterministic layout and an SVG drawing.
//!
//! [`GraphSpec`] is the payload a surface's `graph` node resolves to. The
//! layout is a Fruchterman-Reingold force-directed one with no randomness:
//! the start positions come from an FNV-1a hash of each node id, the
//! iteration count is fixed, and only IEEE-exact arithmetic (`+ - * /` and
//! `sqrt`) is used, so the same spec gives the same layout on every host,
//! `wasm32` included.
//!
//! [`render_graph_svg`] draws edges as `<line>`s, then one
//! `<g class="koine-graph-node" data-id="…">` per node holding a `<circle>`
//! and a `<text>` label where it fits. The `data-id` is how a page learns
//! which node a click landed on.
//!
//! The drawing is legible before it is complete (plan-koine-affordances
//! A-D10): the layout's repulsion counts each node's label as part of its
//! size, so labelled nodes push each other apart by their words, not their
//! dots; and labels are drawn greedily, highest degree first, each only where
//! its box overlaps no label already drawn. A node whose label did not fit
//! carries it as an SVG `<title>` (a hover shows it), and the `<svg>` has a
//! `viewBox`, so a page can scale or pinch-zoom the map to read the rest.
//! Past [`LABEL_LIMIT`] nodes no label is drawn and every node has a title.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::svg::escape_xml;
use crate::PlotError;

/// A graph with more nodes than this is drawn without labels.
pub const LABEL_LIMIT: usize = 500;

/// Fixed iteration count of the layout.
const ITERATIONS: usize = 200;

/// A label's font size, in the drawing's units.
const LABEL_FONT: f64 = 10.0;
/// The width one character of a label is taken to be: 0.6em, a generous
/// average for a proportional sans-serif, so a label's box is not smaller
/// than what the browser draws.
const LABEL_CHAR: f64 = 0.6 * LABEL_FONT;
/// The gap between a node's circle and its label's baseline.
const LABEL_GAP: f64 = 4.0;

/// The width a label is taken to be when it is drawn: [`LABEL_CHAR`] per
/// character.
pub fn label_width(label: &str) -> f64 {
    label.chars().count() as f64 * LABEL_CHAR
}

/// A label's box: left, top, right, bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl LabelBox {
    /// The box of `label` drawn centred on `x` with its baseline at `y`
    /// (`text-anchor="middle"`), at [`LABEL_FONT`]: from one em above the
    /// baseline to a quarter em below it.
    pub fn of(label: &str, x: f64, y: f64) -> Self {
        let half = label_width(label) / 2.0;
        Self {
            x0: x - half,
            y0: y - LABEL_FONT,
            x1: x + half,
            y1: y + LABEL_FONT * 0.25,
        }
    }

    /// Whether the two boxes share any area (touching edges do not).
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

const DEFAULT_WIDTH: f64 = 640.0;
const DEFAULT_HEIGHT: f64 = 480.0;

/// One node. `label` falls back to `id` when drawn.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// One edge, by node id. An edge naming an unknown node is ignored.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
}

/// The graph spec. Missing fields take their defaults, so `{}` is an empty
/// 640 x 480 graph.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphSpec {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub width: f64,
    pub height: f64,
}

impl Default for GraphSpec {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        }
    }
}

/// Parse a graph spec held as JSON.
pub fn parse_graph(value: &Value) -> Result<GraphSpec, PlotError> {
    serde_json::from_value(value.clone()).map_err(|e| PlotError::Spec(e.to_string()))
}

/// Draw a JSON graph spec as a standalone SVG string.
pub fn render_graph_svg_json(value: &Value) -> Result<String, PlotError> {
    Ok(render_graph_svg(&parse_graph(value)?))
}

fn dims(spec: &GraphSpec) -> (f64, f64) {
    let ok = |v: f64, d: f64| if v.is_finite() && v > 0.0 { v } else { d };
    (
        ok(spec.width, DEFAULT_WIDTH),
        ok(spec.height, DEFAULT_HEIGHT),
    )
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A unit fraction in `[0, 1)` from the top 24 bits of a 64-bit hash.
fn unit(h: u64) -> f64 {
    ((h >> 40) as f64) / ((1u64 << 24) as f64)
}

/// A second, independent hash from the first (splitmix64 finaliser).
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The label a node is drawn with: its own, else its id (the first node of a
/// duplicated id wins).
fn label_of<'a>(spec: &'a GraphSpec, id: &'a str) -> &'a str {
    spec.nodes
        .iter()
        .find(|n| n.id == id)
        .and_then(|n| n.label.as_deref())
        .unwrap_or(id)
}

/// Lay the graph out: one `(id, x, y)` per node (the first of any duplicated
/// id), in node order, inside the `width` x `height` area with a margin.
///
/// Repulsion is label-aware: while labels are drawn (up to [`LABEL_LIMIT`]
/// nodes) a node's radius is half its label's width ([`label_width`]), and
/// two nodes repel as if the distance between them were the gap between
/// those radii (never less than a tenth of the distance), so two labelled
/// nodes are pushed apart until their words, not only their dots, clear.
///
/// 0 nodes give an empty list, 1 node the centre. Deterministic.
pub fn layout(spec: &GraphSpec) -> Vec<(String, f64, f64)> {
    let (w, h) = dims(spec);

    // Unique ids, in order.
    let mut ids: Vec<&str> = Vec::with_capacity(spec.nodes.len());
    let mut index = std::collections::HashMap::new();
    for n in &spec.nodes {
        if !index.contains_key(n.id.as_str()) {
            index.insert(n.id.as_str(), ids.len());
            ids.push(n.id.as_str());
        }
    }
    let n = ids.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![(ids[0].to_string(), w / 2.0, h / 2.0)];
    }

    // The box nodes stay in.
    let margin = 24.0_f64.min(w / 4.0).min(h / 4.0);
    let (x0, x1, y0, y1) = (margin, w - margin, margin, h - margin);
    let (bw, bh) = (x1 - x0, y1 - y0);

    let mut pos: Vec<(f64, f64)> = ids
        .iter()
        .map(|id| {
            let a = fnv1a(id.as_bytes());
            let b = mix(a);
            (x0 + unit(a) * bw, y0 + unit(b) * bh)
        })
        .collect();

    // Edges between known, distinct nodes.
    let edges: Vec<(usize, usize)> = spec
        .edges
        .iter()
        .filter_map(|e| {
            let s = *index.get(e.source.as_str())?;
            let t = *index.get(e.target.as_str())?;
            (s != t).then_some((s, t))
        })
        .collect();

    // Each node's size: half its label's width while labels are drawn.
    let labelled = n <= LABEL_LIMIT;
    let size: Vec<f64> = ids
        .iter()
        .map(|id| {
            if labelled {
                label_width(label_of(spec, id)) / 2.0
            } else {
                0.0
            }
        })
        .collect();

    let k = ((bw * bh) / n as f64).sqrt();
    let t0 = bw.min(bh) / 8.0;
    let mut disp = vec![(0.0_f64, 0.0_f64); n];

    for iter in 0..ITERATIONS {
        let temp = t0 * (1.0 - iter as f64 / ITERATIONS as f64);
        for d in disp.iter_mut() {
            *d = (0.0, 0.0);
        }

        // Repulsion between every pair.
        for i in 0..n {
            for j in (i + 1)..n {
                let (mut dx, mut dy) = (pos[i].0 - pos[j].0, pos[i].1 - pos[j].1);
                let mut dist = (dx * dx + dy * dy).sqrt();
                if dist < 1e-6 {
                    // Coincident: separate along a direction fixed by the pair.
                    let u = unit(mix((i as u64) << 32 | j as u64));
                    dx = u - 0.5 + 1e-3;
                    dy = 0.5 - u + 2e-3;
                    dist = (dx * dx + dy * dy).sqrt();
                }
                // The gap between the two labels, not the centres.
                let gap = (dist - size[i] - size[j]).max(dist * 0.1);
                let f = k * k / gap;
                let (fx, fy) = (dx / dist * f, dy / dist * f);
                disp[i].0 += fx;
                disp[i].1 += fy;
                disp[j].0 -= fx;
                disp[j].1 -= fy;
            }
        }

        // Attraction along edges.
        for &(s, t) in &edges {
            let (dx, dy) = (pos[s].0 - pos[t].0, pos[s].1 - pos[t].1);
            let dist = (dx * dx + dy * dy).sqrt().max(1e-6);
            let f = dist * dist / k;
            let (fx, fy) = (dx / dist * f, dy / dist * f);
            disp[s].0 -= fx;
            disp[s].1 -= fy;
            disp[t].0 += fx;
            disp[t].1 += fy;
        }

        // A weak pull to the centre keeps disconnected pieces from drifting
        // to the walls.
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        for i in 0..n {
            disp[i].0 += (cx - pos[i].0) * 0.1;
            disp[i].1 += (cy - pos[i].1) * 0.1;
        }

        // Move, limited by the temperature, and keep inside the box.
        for i in 0..n {
            let (dx, dy) = disp[i];
            let len = (dx * dx + dy * dy).sqrt();
            if len > 0.0 {
                let step = len.min(temp);
                pos[i].0 += dx / len * step;
                pos[i].1 += dy / len * step;
            }
            pos[i].0 = pos[i].0.clamp(x0, x1);
            pos[i].1 = pos[i].1.clamp(y0, y1);
        }
    }

    ids.iter()
        .zip(pos)
        .map(|(id, (x, y))| (id.to_string(), x, y))
        .collect()
}

/// Draw a graph as a standalone `<svg>` string.
///
/// Labels are placed greedily: nodes by degree, highest first (ties in node
/// order), each label centred above its node (below it when there is no room
/// above, and moved in from a side edge) and drawn only when its
/// [`LabelBox`] overlaps no label already drawn. A node whose label is not
/// drawn has it as a `<title>`.
pub fn render_graph_svg(spec: &GraphSpec) -> String {
    let (w, h) = dims(spec);
    let placed = layout(spec);
    let labelled = placed.len() <= LABEL_LIMIT;
    let radius = if labelled { 6.0 } else { 3.0 };

    let mut out = vec![
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" font-family="system-ui, -apple-system, sans-serif">"#
        ),
        format!(r##"<rect x="0.00" y="0.00" width="{w:.2}" height="{h:.2}" fill="#ffffff"/>"##),
    ];

    let at: std::collections::HashMap<&str, (f64, f64)> = placed
        .iter()
        .map(|(id, x, y)| (id.as_str(), (*x, *y)))
        .collect();

    for e in &spec.edges {
        if e.source == e.target {
            continue;
        }
        if let (Some(&(x1, y1)), Some(&(x2, y2))) =
            (at.get(e.source.as_str()), at.get(e.target.as_str()))
        {
            out.push(format!(
                r##"<line x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="#999999" stroke-width="1"/>"##
            ));
        }
    }

    // Which labels are drawn, and where: highest degree first.
    let shown = if labelled {
        place_labels(spec, &placed, radius, w)
    } else {
        vec![None; placed.len()]
    };

    for ((id, x, y), label_at) in placed.iter().zip(shown) {
        let eid = escape_xml(id);
        let text = escape_xml(label_of(spec, id));
        out.push(format!(r#"<g class="koine-graph-node" data-id="{eid}">"#));
        if label_at.is_none() {
            out.push(format!("<title>{text}</title>"));
        }
        out.push(format!(
            r##"<circle cx="{x:.2}" cy="{y:.2}" r="{radius:.1}" fill="#4c78a8" stroke="white" stroke-width="0.8"/>"##
        ));
        if let Some((lx, ly)) = label_at {
            out.push(format!(
                r##"<text x="{lx:.2}" y="{ly:.2}" text-anchor="middle" font-size="10px" fill="#333333" dominant-baseline="auto">{text}</text>"##
            ));
        }
        out.push("</g>".to_string());
    }

    out.push("</svg>".to_string());
    out.join("\n")
}

/// The label position of each placed node (`(x, baseline y)`), or `None`
/// where it would overlap a label of a node of higher degree.
fn place_labels(
    spec: &GraphSpec,
    placed: &[(String, f64, f64)],
    radius: f64,
    width: f64,
) -> Vec<Option<(f64, f64)>> {
    let index: std::collections::HashMap<&str, usize> = placed
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| (id.as_str(), i))
        .collect();
    let mut degree = vec![0usize; placed.len()];
    for e in &spec.edges {
        if e.source == e.target {
            continue;
        }
        if let (Some(&s), Some(&t)) = (index.get(e.source.as_str()), index.get(e.target.as_str())) {
            degree[s] += 1;
            degree[t] += 1;
        }
    }
    let mut order: Vec<usize> = (0..placed.len()).collect();
    // Stable: equal degrees keep node order.
    order.sort_by(|a, b| degree[*b].cmp(&degree[*a]));

    let mut drawn: Vec<LabelBox> = Vec::new();
    let mut out = vec![None; placed.len()];
    for i in order {
        let (id, x, y) = &placed[i];
        let label = label_of(spec, id);
        let half = label_width(label) / 2.0;
        // Moved in from a side edge when it would run off it.
        let lx = if 2.0 * half >= width {
            width / 2.0
        } else {
            x.clamp(half, width - half)
        };
        // Above the dot; below it when there is no room above.
        let above = y - radius - LABEL_GAP;
        let ly = if above - LABEL_FONT < 0.0 {
            y + radius + LABEL_GAP + LABEL_FONT
        } else {
            above
        };
        let rect = LabelBox::of(label, lx, ly);
        if drawn.iter().all(|other| !rect.overlaps(other)) {
            drawn.push(rect);
            out[i] = Some((lx, ly));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str) -> GraphNode {
        GraphNode {
            id: id.into(),
            label: None,
        }
    }

    fn edge(s: &str, t: &str) -> GraphEdge {
        GraphEdge {
            source: s.into(),
            target: t.into(),
        }
    }

    fn triangle() -> GraphSpec {
        GraphSpec {
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![edge("a", "b"), edge("b", "c"), edge("c", "a")],
            ..GraphSpec::default()
        }
    }

    #[test]
    fn defaults_are_640_by_480() {
        let g = parse_graph(&json!({})).unwrap();
        assert_eq!((g.width, g.height), (640.0, 480.0));
        assert!(g.nodes.is_empty() && g.edges.is_empty());
        let g = parse_graph(&json!({"nodes": [{"id": "x"}]})).unwrap();
        assert_eq!(g.nodes[0].label, None);
    }

    #[test]
    fn layout_edge_cases() {
        assert!(layout(&GraphSpec::default()).is_empty());
        let one = GraphSpec {
            nodes: vec![node("solo")],
            ..GraphSpec::default()
        };
        assert_eq!(layout(&one), vec![("solo".to_string(), 320.0, 240.0)]);
    }

    #[test]
    fn layout_is_deterministic() {
        let g = triangle();
        assert_eq!(layout(&g), layout(&g));
        assert_eq!(layout(&g), layout(&g.clone()));
    }

    #[test]
    fn layout_stays_in_bounds_and_isolated_nodes_do_not_fly_off() {
        let mut g = triangle();
        g.nodes.push(node("loner"));
        g.nodes.push(node("loner2"));
        g.edges.push(edge("a", "ghost")); // unknown node: ignored
        let placed = layout(&g);
        assert_eq!(placed.len(), 5);
        for (id, x, y) in &placed {
            assert!(
                (24.0..=616.0).contains(x) && (24.0..=456.0).contains(y),
                "{id} at ({x},{y})"
            );
        }
        // Distinct nodes do not pile onto one point.
        for i in 0..placed.len() {
            for j in (i + 1)..placed.len() {
                let (dx, dy) = (placed[i].1 - placed[j].1, placed[i].2 - placed[j].2);
                assert!(
                    dx * dx + dy * dy > 1.0,
                    "{} and {}",
                    placed[i].0,
                    placed[j].0
                );
            }
        }
    }

    #[test]
    fn connected_nodes_end_up_closer_than_the_area() {
        let g = triangle();
        let p = layout(&g);
        let d = |i: usize, j: usize| ((p[i].1 - p[j].1).powi(2) + (p[i].2 - p[j].2).powi(2)).sqrt();
        assert!(d(0, 1) < 400.0 && d(1, 2) < 400.0 && d(0, 2) < 400.0);
    }

    #[test]
    fn triangle_renders_three_lines_and_three_nodes() {
        let svg = render_graph_svg(&triangle());
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert_eq!(svg.matches("<line").count(), 3);
        assert_eq!(svg.matches(r#"class="koine-graph-node""#).count(), 3);
        for id in ["a", "b", "c"] {
            assert!(svg.contains(&format!(r#"data-id="{id}""#)), "{id}");
        }
        assert_eq!(svg.matches("<circle").count(), 3);
        assert_eq!(svg.matches("<text").count(), 3);
        // Edges are drawn before nodes.
        assert!(svg.rfind("<line").unwrap() < svg.find("<g class").unwrap());
    }

    #[test]
    fn unknown_edges_are_not_drawn_and_labels_replace_ids() {
        let g = GraphSpec {
            nodes: vec![GraphNode {
                id: "n1".into(),
                label: Some("Node <one>".into()),
            }],
            edges: vec![edge("n1", "missing")],
            ..GraphSpec::default()
        };
        let svg = render_graph_svg(&g);
        assert_eq!(svg.matches("<line").count(), 0);
        assert!(svg.contains("Node &lt;one&gt;"));
        assert!(!svg.contains(">n1<"));
    }

    #[test]
    fn ids_are_escaped_in_attributes() {
        let g = GraphSpec {
            nodes: vec![node(r#"a"b&c"#)],
            ..GraphSpec::default()
        };
        let svg = render_graph_svg(&g);
        assert!(svg.contains(r#"data-id="a&quot;b&amp;c""#), "{svg}");
    }

    #[test]
    fn more_than_500_nodes_render_without_labels() {
        let g = GraphSpec {
            nodes: (0..501).map(|i| node(&format!("n{i}"))).collect(),
            ..GraphSpec::default()
        };
        let svg = render_graph_svg(&g);
        assert_eq!(svg.matches("koine-graph-node").count(), 501);
        assert!(!svg.contains("<text"));
        // Every node still names itself, as a title.
        assert_eq!(svg.matches("<title>").count(), 501);
        // At 500, labels are drawn where they fit and the rest are titles.
        let g500 = GraphSpec {
            nodes: (0..500).map(|i| node(&format!("n{i}"))).collect(),
            ..GraphSpec::default()
        };
        let svg = render_graph_svg(&g500);
        let drawn = svg.matches("<text").count();
        assert!(drawn > 0);
        assert_eq!(drawn + svg.matches("<title>").count(), 500);
    }

    /// Every drawn label's box, read back off the SVG.
    fn drawn_labels(svg: &str) -> Vec<(String, LabelBox)> {
        svg.split("<text ")
            .skip(1)
            .map(|rest| {
                let num = |name: &str| -> f64 {
                    let at = rest.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
                    rest[at..at + rest[at..].find('"').unwrap()]
                        .parse()
                        .unwrap()
                };
                let open = rest.find('>').unwrap() + 1;
                let text = rest[open..open + rest[open..].find("</text>").unwrap()].to_string();
                let label = text
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&");
                let rect = LabelBox::of(&label, num("x"), num("y"));
                (label, rect)
            })
            .collect()
    }

    /// A dense map: a hub, a ring of spokes with long labels, a few chords.
    fn dense() -> GraphSpec {
        let mut nodes = vec![GraphNode {
            id: "hub".into(),
            label: Some("surface-demo-service".into()),
        }];
        let mut edges = Vec::new();
        for i in 0..40 {
            nodes.push(GraphNode {
                id: format!("n{i}"),
                label: Some(format!("surface-demo-service_verb-{i}")),
            });
            edges.push(edge("hub", &format!("n{i}")));
            edges.push(edge(&format!("n{i}"), &format!("n{}", (i + 1) % 40)));
        }
        GraphSpec {
            nodes,
            edges,
            ..GraphSpec::default()
        }
    }

    #[test]
    fn no_two_drawn_labels_overlap_and_a_skipped_one_is_a_title() {
        let g = dense();
        let svg = render_graph_svg(&g);
        let labels = drawn_labels(&svg);
        assert!(!labels.is_empty());
        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                assert!(
                    !labels[i].1.overlaps(&labels[j].1),
                    "{:?} overlaps {:?}",
                    labels[i],
                    labels[j]
                );
            }
        }
        // Each node has its label once: drawn, or as a title.
        let titles = svg.matches("<title>").count();
        assert_eq!(labels.len() + titles, g.nodes.len(), "{svg}");
        assert!(titles > 0, "the dense map cannot draw every label");
        // The hub has the highest degree: its label is drawn first, so it is
        // drawn at all.
        assert!(labels.iter().any(|(l, _)| l == "surface-demo-service"));
        // Every drawn label stays inside the drawing, sideways.
        for (label, rect) in &labels {
            assert!(
                rect.x0 >= -1e-9 && rect.x1 <= 640.0 + 1e-9,
                "{label} {rect:?}"
            );
            assert!(rect.y0 >= 0.0, "{label} {rect:?}");
        }
        assert!(svg.contains(r#"viewBox="0 0 640 480""#));
    }

    #[test]
    fn labels_count_in_the_layouts_repulsion() {
        // Two linked nodes with long labels end up further apart than the
        // same pair with one-letter labels.
        let pair = |a: &str, b: &str| GraphSpec {
            nodes: vec![
                GraphNode {
                    id: "a".into(),
                    label: Some(a.into()),
                },
                GraphNode {
                    id: "b".into(),
                    label: Some(b.into()),
                },
            ],
            edges: vec![edge("a", "b")],
            ..GraphSpec::default()
        };
        let gap = |g: &GraphSpec| {
            let p = layout(g);
            ((p[0].1 - p[1].1).powi(2) + (p[0].2 - p[1].2).powi(2)).sqrt()
        };
        let short = gap(&pair("a", "b"));
        let long = gap(&pair(
            "a very long label on the left",
            "and another long label",
        ));
        assert!(long > short, "{long} <= {short}");
        assert_eq!(label_width("abcde"), 30.0);
    }

    #[test]
    fn parse_errors_are_spec_errors() {
        for bad in [
            json!("nope"),
            json!({"nodes": [{"label": "no id"}]}),
            json!({"edges": [{"source": "a"}]}),
            json!({"width": "wide"}),
        ] {
            assert!(
                matches!(parse_graph(&bad), Err(PlotError::Spec(_))),
                "{bad}"
            );
            assert!(matches!(
                render_graph_svg_json(&bad),
                Err(PlotError::Spec(_))
            ));
        }
        assert!(render_graph_svg_json(&json!({"nodes": [{"id": "a"}]})).is_ok());
    }
}
