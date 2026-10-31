//! Federation (S-D7): every selected source asked concurrently, each under
//! its own timeout; rows merged by identifier only; order by the requested
//! sort, else the first-listed source's order.
//!
//! **Why a `JoinSet` and not a koine-tasks job.** The inline job runner
//! (`koine_tasks::inline::start_inline`) mints a `task@1.0.0` row and writes
//! its event ring through a store handle, and this service has no store
//! port by design (S-D6's local-store step is deliberately absent). No
//! linked service runs a verb as a job without one. So the fan-out is a
//! `tokio::task::JoinSet` here, and the job *narration* is kept: each
//! source's page is handed to `impress_service_core::job::emit` as it
//! arrives, which is a no-op outside a job and the per-source stream the
//! plan asks for when a host runs the verb `in_background`.
//!
//! One source's failure is never the call's: a timeout is that source's
//! `timed_out` diagnostic, a panic or transport failure its `error`, and the
//! other sources' rows still return.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use koine_sources::cascade::{source_of, source_priority};
use koine_sources::{Diagnostic, Status, Work};
use serde_json::json;
use tokio::task::JoinSet;

use crate::adapter::{Adapter, Page};
use crate::etiquette::Gate;
use crate::query::{all_done, shares_id};

/// The per-source timeout a call gets unless it passes `timeout_ms`.
pub const DEFAULT_TIMEOUT_MS: u64 = 8_000;

/// How a merge is done; the one word `explain` reports.
pub const MERGE: &str = "identifier";

/// One source's answer under its timeout: `Err` is the timeout elapsed.
type Timed = Result<Result<Page, Diagnostic>, tokio::time::error::Elapsed>;

/// What one source answered, in the order the sources were listed.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub source: String,
    pub rows: Vec<Work>,
    pub next: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// The work one source does, boxed so a `search`, a `fetch` and a `related`
/// all fan out through [`run_all`].
pub type SourceFuture = Pin<Box<dyn Future<Output = Result<Page, Diagnostic>> + Send>>;

/// Ask every adapter concurrently, each through `op` (given its index in
/// `adapters` and itself) under `timeout`, after the gate admits it. The outcomes come back in the adapters' order; a
/// source the gate refused has its `rate_limited` diagnostic and no rows.
/// Every source's outcome carries exactly one status line for the source
/// (`done` when its page was clean) plus whatever the page reported.
pub async fn run_all<F>(
    adapters: &[Arc<dyn Adapter>],
    gate: &Gate,
    timeout: Duration,
    op: F,
) -> Vec<Outcome>
where
    F: Fn(usize, Arc<dyn Adapter>) -> SourceFuture,
{
    let mut outcomes: Vec<Outcome> = adapters
        .iter()
        .map(|a| Outcome {
            source: a.explain().source,
            ..Default::default()
        })
        .collect();
    let mut set: JoinSet<(usize, Timed)> = JoinSet::new();
    // Task id → source index, for the one outcome a `JoinError` carries no
    // index for: a panic.
    let mut tasks: HashMap<tokio::task::Id, usize> = HashMap::new();
    for (i, adapter) in adapters.iter().enumerate() {
        let record = adapter.explain();
        match gate.admit(&record) {
            Ok(warnings) => outcomes[i].diagnostics.extend(warnings),
            Err(refused) => {
                outcomes[i].diagnostics.push(refused);
                continue;
            }
        }
        let fut = op(i, adapter.clone());
        let handle = set.spawn(async move { (i, tokio::time::timeout(timeout, fut).await) });
        tasks.insert(handle.id(), i);
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((i, Ok(Ok(page)))) => {
                let out = &mut outcomes[i];
                let clean = all_done(&page.diagnostics);
                out.diagnostics.extend(page.diagnostics);
                if clean {
                    out.diagnostics.push(Diagnostic::done(&out.source));
                }
                out.next = page.next;
                out.rows = page.rows;
                emit(out);
            }
            Ok((i, Ok(Err(diagnostic)))) => {
                outcomes[i].diagnostics.push(diagnostic);
                emit(&outcomes[i]);
            }
            Ok((i, Err(_elapsed))) => {
                let out = &mut outcomes[i];
                out.diagnostics.push(Diagnostic::new(
                    &out.source,
                    Status::TimedOut,
                    format!("no answer within {}ms", timeout.as_millis()),
                ));
                emit(out);
            }
            Err(join_error) => {
                let what = if join_error.is_panic() {
                    "the adapter panicked".to_string()
                } else {
                    format!("the adapter's task was cancelled: {join_error}")
                };
                if let Some(&i) = tasks.get(&join_error.id()) {
                    let out = &mut outcomes[i];
                    out.diagnostics.push(Diagnostic::error(&out.source, what));
                    emit(out);
                }
            }
        }
    }
    outcomes
}

/// One source's page as a line of the job's stream, when the verb runs as
/// a job; nothing otherwise.
fn emit(outcome: &Outcome) {
    impress_service_core::job::emit(json!({
        "source": outcome.source,
        "rows": outcome.rows,
        "next": outcome.next,
        "diagnostics": outcome.diagnostics,
    }));
}

/// Fold works that are one record into one: the body comes from the source
/// with the best `source_priority`, each empty field falling back to the
/// next source; `ids` and `locations` are unioned, every source's count is
/// kept in `metrics`, and `provenance.sources` lists every contributor.
/// Among equal priorities the first given wins.
///
/// The title is the one exception to "the body is one source's": a title
/// set in capitals throughout (Crossref's deposits for World Scientific
/// chapters and older ApJ papers) gives way to the first contributor, in
/// the same order, whose title is not. Priority ranks a source's metadata,
/// not its typography, and the same work's title in another source is the
/// same words properly cased.
///
/// Among one source's records of a work (a preprint and its published
/// version, joined by [`same_paper`]) the published one keeps the body.
pub fn merge_works(works: Vec<Work>) -> Work {
    let mut indexed: Vec<(u32, bool, usize, Work)> = works
        .into_iter()
        .enumerate()
        .map(|(i, w)| (source_priority(source_of(&w)), is_preprint(&w), i, w))
        .collect();
    indexed.sort_by_key(|(priority, preprint, i, _)| (*priority, *preprint, *i));
    let indexed: Vec<(u32, usize, Work)> = indexed
        .into_iter()
        .map(|(priority, _, i, w)| (priority, i, w))
        .collect();
    let cased = indexed
        .iter()
        .filter_map(|(_, _, w)| w.title.as_deref())
        .find(|t| !shouts(t))
        .map(str::to_string);
    let mut it = indexed.into_iter().map(|(_, _, w)| w);
    let Some(mut kept) = it.next() else {
        return Work::default();
    };
    for other in it {
        crate::query::merge(&mut kept, other);
    }
    if kept.title.as_deref().is_some_and(shouts) {
        if let Some(cased) = cased {
            kept.title = Some(cased);
        }
    }
    kept
}

/// A record of a work before its publication: an arXiv DOI, or no venue
/// but arXiv.
fn is_preprint(work: &Work) -> bool {
    let arxiv_doi = |d: &str| d.to_ascii_lowercase().starts_with("10.48550/");
    work.doi.as_deref().is_some_and(arxiv_doi)
        || work.ids.get("doi").is_some_and(|d| arxiv_doi(d))
        || work.container_title.as_deref().is_none_or(|v| {
            let v = v.trim().to_ascii_lowercase();
            // DBLP files arXiv as "CoRR".
            v.is_empty() || v.contains("arxiv") || v == "corr"
        })
}

/// Whether two rows that share no identifier are still one paper: the same
/// title (case, markup and punctuation aside, and at least four words, so
/// an "Introduction" is never anyone else's), the same first author's
/// surname (read leniently: [`surname_in`]), and years at most one apart. That is a preprint and its
/// published version when a source knows only one of the two identifiers:
/// OpenAlex lists both records of "The Power Spectrum Dependence of Dark
/// Matter Halo Concentrations", 2000 and 2001, and a search showed both.
pub fn same_paper(a: &Work, b: &Work) -> bool {
    let (Some(ta), Some(tb)) = (a.title.as_deref(), b.title.as_deref()) else {
        return false;
    };
    let ka = title_words(ta);
    if ka.len() < 4 || ka != title_words(tb) {
        return false;
    }
    let same_first_author = match (a.author.first(), b.author.first()) {
        (Some(x), Some(y)) => surname_in(x, y) || surname_in(y, x),
        _ => false,
    };
    match (year_of(a), year_of(b)) {
        (Some(ya), Some(yb)) => same_first_author && (ya - yb).abs() <= 1,
        _ => false,
    }
}

/// Whether `a`'s surname is one of the words of `b`'s whole name. Not
/// surname to surname: a source that misreads "V. R. Eke" stores family
/// "R", given "Eke, V" (OpenAlex's preprint record of Eke et al. 2001), and
/// the name is still Eke's. An initial is never a surname here.
fn surname_in(a: &koine_sources::kind::Author, b: &koine_sources::kind::Author) -> bool {
    let surname = title_words(a.family.as_deref().unwrap_or(""));
    if surname.is_empty() || surname.iter().all(|w| w.chars().count() < 2) {
        return false;
    }
    let whole: Vec<String> = [b.family.as_deref(), b.given.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(title_words)
        .collect();
    surname.iter().all(|w| whole.contains(w))
}

/// A title's words, lower case, with markup (`<sup>`) and punctuation gone.
fn title_words(title: &str) -> Vec<String> {
    let mut text = String::with_capacity(title.len());
    let mut in_tag = false;
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' if chars
                .peek()
                .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/') =>
            {
                in_tag = true
            }
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            _ => text.extend(c.to_lowercase()),
        }
    }
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether a title is set in capitals throughout: letters, and none of
/// them lower case. A title of four letters or fewer (`GADGET`, `ALMA`
/// aside) is too short to tell an acronym from shouting, so it never is.
pub fn shouts(title: &str) -> bool {
    let letters = title.chars().filter(|c| c.is_alphabetic()).count();
    letters > 4 && !title.chars().any(char::is_lowercase)
}

/// A row seen in one source: where, and at which position.
#[derive(Debug, Clone)]
struct Seen {
    /// Index of the source in the call's list.
    listed: usize,
    /// Position of the row in that source's page.
    pos: usize,
    priority: u32,
}

#[derive(Debug)]
struct Entry {
    contributions: Vec<Work>,
    seen: Vec<Seen>,
}

impl Entry {
    /// The row's first source in listing order, and its position there.
    fn first(&self) -> &Seen {
        self.seen
            .iter()
            .min_by_key(|s| (s.listed, s.pos))
            .expect("an entry has at least one contribution")
    }

    /// The relevance key (S-D7): position in the first source, ties by priority.
    fn relevance(&self) -> (usize, u32, usize) {
        let first = self.first();
        (first.pos, first.priority, first.listed)
    }
}

/// The sorts the service applies across the merged rows.
pub const SORTS: &[&str] = &["relevance", "year", "cited_by"];

/// Merge the outcomes' rows by identifier (or, sharing none, by
/// [`same_paper`]) and order them. With `sort`, the
/// order is across every merged row (`year` and `cited_by` descending,
/// unknown last; `relevance` the row's position in its first source, ties
/// by source priority); without, the first-listed source's rows in its
/// order, then the other sources' in source-priority order, a row placed
/// where it was first seen.
pub fn merge_rows(outcomes: &[Outcome], sort: Option<&str>) -> Vec<Work> {
    // Rank: the first-listed source first, the rest by priority then listing.
    let mut rank: Vec<usize> = (0..outcomes.len()).collect();
    rank.sort_by_key(|&i| {
        if i == 0 {
            (0, 0, 0)
        } else {
            (1, source_priority(&outcomes[i].source), i)
        }
    });
    let mut entries: Vec<Entry> = Vec::new();
    for &listed in &rank {
        let outcome = &outcomes[listed];
        let priority = source_priority(&outcome.source);
        for (pos, row) in outcome.rows.iter().enumerate() {
            let seen = Seen {
                listed,
                pos,
                priority,
            };
            match entries.iter_mut().find(|e| {
                e.contributions
                    .iter()
                    .any(|c| shares_id(c, row) || same_paper(c, row))
            }) {
                Some(entry) => {
                    entry.contributions.push(row.clone());
                    entry.seen.push(seen);
                }
                None => entries.push(Entry {
                    contributions: vec![row.clone()],
                    seen: vec![seen],
                }),
            }
        }
    }
    let mut merged: Vec<(Entry, Work)> = entries
        .into_iter()
        .map(|e| {
            let work = merge_works(e.contributions.clone());
            (e, work)
        })
        .collect();
    match sort {
        Some("relevance") => merged.sort_by_key(|(e, _)| e.relevance()),
        Some("year") => merged
            .sort_by_key(|(e, w)| (year_of(w).map_or(i64::MAX, |y| -(y as i64)), e.relevance())),
        Some("cited_by") => merged.sort_by_key(|(e, w)| {
            (
                w.metrics
                    .as_ref()
                    .and_then(|m| m.cited_by.as_ref())
                    .map_or(i128::MAX, |c| -(c.value as i128)),
                e.relevance(),
            )
        }),
        _ => {}
    }
    merged.into_iter().map(|(_, w)| w).collect()
}

/// The year a work was issued, when it says.
pub fn year_of(work: &Work) -> Option<i32> {
    work.issued
        .as_ref()
        .and_then(|i| i.date_parts.first())
        .and_then(|p| p.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koine_sources::kind::{Counted, Location, Metrics, Provenance, Version};
    use std::collections::BTreeMap;

    fn work(source: &str, doi: &str, title: &str, year: i32, cited: u64) -> Work {
        Work {
            ty: "article-journal".into(),
            title: Some(title.into()),
            issued: Some(koine_sources::kind::Issued {
                date_parts: vec![vec![year]],
            }),
            ids: BTreeMap::from([("doi".to_string(), doi.to_string())]),
            locations: vec![Location {
                source: source.into(),
                source_id: doi.into(),
                version: Version::Published,
                url: Some(format!("https://{source}.example/{doi}")),
                pdf: None,
                license: None,
                oa_status: None,
                primary: true,
            }],
            provenance: Some(Provenance {
                fetched_at: "2026-10-19T00:00:00Z".into(),
                source: source.into(),
                source_id: doi.into(),
                query: None,
                sources: vec![],
            }),
            metrics: Some(Metrics {
                cited_by: Some(Counted {
                    value: cited,
                    source: source.into(),
                }),
                cited_by_all: vec![],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_priority_source_keeps_the_body_and_every_count_is_kept() {
        // openalex (50) listed before crossref (10): crossref's title still wins.
        let merged = merge_works(vec![
            work("openalex", "10.1/A", "From OpenAlex", 2020, 7),
            work("crossref", "10.1/a", "From Crossref", 2020, 5),
        ]);
        assert_eq!(merged.title.as_deref(), Some("From Crossref"));
        assert_eq!(merged.locations.len(), 2);
        let m = merged.metrics.unwrap();
        assert_eq!(m.cited_by.unwrap().source, "crossref");
        assert_eq!(m.cited_by_all.len(), 2);
        assert_eq!(
            merged.provenance.unwrap().sources,
            vec!["crossref".to_string(), "openalex".to_string()]
        );
    }

    #[test]
    fn a_title_in_capitals_gives_way_to_a_cased_one() {
        // crossref (10) keeps the body; ads (30) has the title cased.
        let merged = merge_works(vec![
            work("ads", "10.1/x", "The Dark Matter Halo of M31", 2013, 5),
            work("crossref", "10.1/x", "THE DARK MATTER HALO OF M31", 2013, 7),
        ]);
        assert_eq!(merged.title.as_deref(), Some("The Dark Matter Halo of M31"));
        assert_eq!(source_of(&merged), "crossref");
        // With no cased title anywhere, the priority source's stands.
        let merged = merge_works(vec![
            work("ads", "10.1/y", "WEIGHING THE HALO", 2005, 1),
            work("crossref", "10.1/y", "WEIGHING THE HALO!", 2005, 1),
        ]);
        assert_eq!(merged.title.as_deref(), Some("WEIGHING THE HALO!"));
        assert!(!shouts("ALMA"));
        assert!(!shouts("21cm maps of HI"));
    }

    #[test]
    fn a_preprint_and_its_published_version_are_one_row() {
        let by = |w: Work, family: &str| Work {
            author: vec![koine_sources::kind::Author {
                family: Some(family.into()),
                ..Default::default()
            }],
            ..w
        };
        let title = "The Power Spectrum Dependence of Dark Matter Halo Concentrations";
        let published = Work {
            container_title: Some("The Astrophysical Journal".into()),
            ..by(work("openalex", "10.1086/318245", title, 2001, 486), "Eke")
        };
        // OpenAlex's own preprint record misreads "V. R. Eke" as family "R".
        let preprint = Work {
            author: vec![koine_sources::kind::Author {
                family: Some("R".into()),
                given: Some("Eke, V".into()),
                ..Default::default()
            }],
            ..work(
                "openalex",
                "10.48550/arxiv.astro-ph/0012337",
                &title.to_uppercase(),
                2000,
                304,
            )
        };
        let other_author = by(work("openalex", "10.1/z", title, 2001, 3), "Navarro");
        let ten_years_on = by(work("openalex", "10.1/later", title, 2011, 3), "Eke");
        let outcomes = vec![Outcome {
            source: "openalex".into(),
            rows: vec![preprint, published, other_author, ten_years_on],
            ..Default::default()
        }];
        let rows = merge_rows(&outcomes, None);
        assert_eq!(
            rows.len(),
            3,
            "only the preprint folds into its published version"
        );
        // The published record keeps the body, though the preprint came first.
        assert_eq!(rows[0].ids["doi"], "10.1086/318245");
        assert_eq!(year_of(&rows[0]), Some(2001));
        // Short titles never fold on their words alone.
        assert!(!same_paper(
            &by(work("a", "10.1/a", "Introduction", 2001, 0), "Eke"),
            &by(work("b", "10.1/b", "Introduction", 2001, 0), "Eke"),
        ));
    }

    #[test]
    fn rows_order_by_the_first_source_then_priority_or_by_the_sort() {
        let outcomes = vec![
            Outcome {
                source: "openalex".into(),
                rows: vec![
                    work("openalex", "10.1/x", "X", 2019, 1),
                    work("openalex", "10.1/y", "Y", 2022, 9),
                ],
                ..Default::default()
            },
            Outcome {
                source: "arxiv".into(),
                rows: vec![work("arxiv", "10.1/z", "Z", 2024, 0)],
                ..Default::default()
            },
            Outcome {
                source: "crossref".into(),
                rows: vec![
                    work("crossref", "10.1/w", "W", 2021, 3),
                    work("crossref", "10.1/Y", "Y again", 2022, 4),
                ],
                ..Default::default()
            },
        ];
        let titles = |rows: &[Work]| -> Vec<String> {
            rows.iter().map(|r| r.title.clone().unwrap()).collect()
        };
        // No sort: openalex first (listed first), then crossref (10) before arxiv (60).
        let rows = merge_rows(&outcomes, None);
        assert_eq!(titles(&rows), ["X", "Y again", "W", "Z"]);
        assert_eq!(rows.len(), 4, "Y merged into one row");
        let rows = merge_rows(&outcomes, Some("year"));
        assert_eq!(titles(&rows), ["Z", "Y again", "W", "X"]);
        let rows = merge_rows(&outcomes, Some("cited_by"));
        assert_eq!(titles(&rows), ["Y again", "W", "X", "Z"]);
        // Relevance: position in the first source; ties by priority.
        let rows = merge_rows(&outcomes, Some("relevance"));
        assert_eq!(titles(&rows), ["W", "X", "Z", "Y again"]);
    }
}
