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
//! and (up to [`LABEL_LIMIT`] nodes) a `<text>` label. The `data-id` is how a
//! page learns which node a click landed on.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::svg::escape_xml;
use crate::PlotError;

/// A graph with more nodes than this is drawn without labels.
pub const LABEL_LIMIT: usize = 500;

/// Fixed iteration count of the layout.
const ITERATIONS: usize = 200;

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

/// Lay the graph out: one `(id, x, y)` per node (the first of any duplicated
/// id), in node order, inside the `width` x `height` area with a margin.
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
                let f = k * k / dist;
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

    // The first node of a duplicated id wins, label included.
    let first_label = |id: &str| -> Option<&str> {
        spec.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.label.as_deref())
    };

    for (id, x, y) in &placed {
        let eid = escape_xml(id);
        out.push(format!(r#"<g class="koine-graph-node" data-id="{eid}">"#));
        out.push(format!(
            r##"<circle cx="{x:.2}" cy="{y:.2}" r="{radius:.1}" fill="#4c78a8" stroke="white" stroke-width="0.8"/>"##
        ));
        if labelled {
            let text = escape_xml(first_label(id).unwrap_or(id));
            out.push(format!(
                r##"<text x="{x:.2}" y="{:.2}" text-anchor="middle" font-size="10px" fill="#333333" dominant-baseline="auto">{text}</text>"##,
                y - radius - 4.0
            ));
        }
        out.push("</g>".to_string());
    }

    out.push("</svg>".to_string());
    out.join("\n")
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
        let g500 = GraphSpec {
            nodes: (0..500).map(|i| node(&format!("n{i}"))).collect(),
            ..GraphSpec::default()
        };
        assert_eq!(render_graph_svg(&g500).matches("<text").count(), 500);
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
