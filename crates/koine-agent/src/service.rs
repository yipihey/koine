//! `agent-service`: the agent desk and the skill (plan-koine-research-os
//! S-D2, G-D1..G-D4).

use std::time::Duration;

use impress_service_core::async_trait;
use impress_service_core::pipeline::{context as call_context, CallerIdentity};
use impress_service_core::{call, VerbDescriptor, VerbHandle};
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use serde_json::{json, Value};

use crate::context::{self, safety_word, verb_row};
use crate::desk::{self, DeskError};
use crate::dto::*;
use crate::skill;

/// The longest `next` waits.
pub const MAX_WAIT_MS: u64 = 30_000;

/// The smallest valid surface, for a `make-surface` request's context.
pub const EXAMPLE_SPEC: &str = r#"{"surface": "1.0", "name": "Hello", "state": {"n": 1}, "sources": {}, "root": {"column": [{"text": "n is {{state.n}}"}, {"button": {"label": "More", "on_click": [{"set": {"path": "state.n", "value": 2}}]}}]}}"#;

#[impress_service]
pub trait AgentService: Send + Sync + 'static {
    /// This app's skill: a `SKILL.md` an agent reads to learn the app — how
    /// to reach it, the rules, the adverbs, the record kinds and every verb
    /// with its safety. Generated from the linked inventory; the same build
    /// answers the same text.
    #[impress_method]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn skill(&self) -> SkillResult;

    /// What the person is pointing at: the focused pane (view kind, role,
    /// query, the surface it shows), what every selection channel carries,
    /// and the verbs that act on the selected kinds. `app_id` defaults to
    /// this host's, `device` to this machine. Answers an empty context, not a
    /// refusal, when there is no layout.
    #[impress_method(effects(reads = ["impress/ui/layout@1.0.0"]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn context(&self, app_id: Option<String>, device: Option<String>) -> ContextResult;

    /// Leave a request for an agent: `prompt` in the person's words, with
    /// what they are pointing at attached (unless `with_context` is false):
    /// the layout's focus and selection, and, given `surface_id`, the surface
    /// they were looking at (its name, spec and current state). Refuses an
    /// empty prompt.
    #[impress_method(
        safety = mutating,
        effects(
            reads = ["impress/ui/layout@1.0.0", "impress/ui/surface@1.0.0"],
            writes = [any("koine/agent-request@1.0.0")]
        )
    )]
    #[impress_example(
        name = "default",
        args = r#"{"prompt": "Which of the selected papers cite each other?", "with_context": false}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn ask(
        &self,
        prompt: String,
        with_context: Option<bool>,
        surface_id: Option<String>,
    ) -> RequestResult;

    /// Hand a call to an agent: a `diagnose` request whose context is the
    /// call's record from the notebook (its steps, its arguments as recorded,
    /// its outcome), the verb's description, safety and input schema, and
    /// `prompt` if given. Refuses a call the notebook has no record of.
    #[impress_method(
        safety = mutating,
        effects(reads = ["core/verb-call@1.0.0"], writes = [any("koine/agent-request@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"call_id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn diagnose(&self, call_id: String, prompt: Option<String>) -> RequestResult;

    /// Ask an agent for a view: a `make-surface` request whose context is
    /// what writing one takes — the record kinds, every verb with its
    /// description and safety, a minimal spec, and the verbs that validate,
    /// store and show it. Refuses an empty description.
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/agent-request@1.0.0")])
    )]
    #[impress_example(
        name = "default",
        args = r#"{"description": "A table of my notes with a button to add one", "name": "Notes table"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn make_surface(&self, description: String, name: Option<String>) -> RequestResult;

    /// The desk, newest first: every request (`status` absent or `all`), or
    /// those with `status` (`open`, `claimed`, `answered`, `declined`), without their contexts
    /// (`next` and `claim` answer one with its context).
    #[impress_method(effects(reads = [any("koine/agent-request@1.0.0")]))]
    #[impress_example(name = "default", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    #[impress_example(
        name = "open",
        args = r#"{"status": "open"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn requests(&self, status: Option<String>) -> RequestsResult;

    /// Wait up to `timeout_ms` (default 0, at most 30000) for an open
    /// request and answer the oldest, with its context; no request when none
    /// arrived. An agent's loop is `next`, `claim`, work, `answer`.
    #[impress_method(effects(reads = [any("koine/agent-request@1.0.0")]))]
    #[impress_example(
        name = "default",
        args = r#"{"timeout_ms": 0}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn next(&self, timeout_ms: Option<u64>) -> RequestResult;

    /// Take an open request: it becomes `claimed` by the caller. Refuses one
    /// that is not open (`conflict`).
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/agent-request@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn claim(&self, id: String) -> RequestResult;

    /// Resolve a request with `text`, naming `surface_id` when the answer is
    /// a surface (the desk then offers to show it). From `open` or `claimed`.
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/agent-request@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000", "text": "Done."}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn answer(&self, id: String, text: String, surface_id: Option<String>) -> RequestResult;

    /// Give a request back with `reason`. From `open` or `claimed`.
    #[impress_method(
        safety = mutating,
        effects(writes = [any("koine/agent-request@1.0.0")])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000", "reason": "Needs a SciX token."}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn decline(&self, id: String, reason: String) -> RequestResult;

    /// Show the surface an answered request names, in a new pane beside the
    /// focused one of this host's window. Refuses a request with no surface.
    #[impress_method(
        safety = mutating,
        effects(reads = [any("koine/agent-request@1.0.0")], writes = ["impress/ui/layout@1.0.0"])
    )]
    #[impress_example(
        name = "unknown",
        args = r#"{"id": "00000000-0000-4000-8000-000000000000"}"#,
        expect = r#"{"ok": false, "code": "not-found"}"#
    )]
    async fn show(&self, id: String) -> ShowResult;
}

#[derive(Clone, Default)]
pub struct DefaultAgentService;

impl DefaultAgentService {
    pub fn new() -> Self {
        Self
    }
}

fn caller() -> CallerIdentity {
    call_context::current()
        .map(|c| c.caller.clone())
        .unwrap_or(CallerIdentity::Person)
}

fn actor_kind(caller: &CallerIdentity) -> impress_item::ActorKind {
    match caller {
        CallerIdentity::Person => impress_item::ActorKind::Human,
        CallerIdentity::Agent(_) => impress_item::ActorKind::Agent,
        _ => impress_item::ActorKind::System,
    }
}

fn refused(error: DeskError) -> RequestResult {
    RequestResult {
        ok: false,
        code: Some(error.code.into()),
        message: Some(error.message),
        ..Default::default()
    }
}

fn filed(result: Result<RequestRow, DeskError>) -> RequestResult {
    match result {
        Ok(row) => RequestResult {
            ok: true,
            request: Some(row),
            ..Default::default()
        },
        Err(e) => refused(e),
    }
}

fn file(kind: &str, prompt: &str, context: Value, call_id: Option<&str>) -> RequestResult {
    let who = caller();
    filed(desk::file(
        kind,
        prompt,
        context,
        call_id,
        who.author(),
        actor_kind(&who),
    ))
}

#[async_trait::async_trait]
impl AgentService for DefaultAgentService {
    async fn skill(&self) -> SkillResult {
        let app = crate::app();
        SkillResult {
            ok: true,
            name: skill::skill_name(&app.app_id),
            markdown: skill::render(&app),
        }
    }

    async fn context(&self, app_id: Option<String>, device: Option<String>) -> ContextResult {
        let app_id = app_id
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| crate::app().app_id);
        context::pointing(&app_id, device).await
    }

    async fn ask(
        &self,
        prompt: String,
        with_context: Option<bool>,
        surface_id: Option<String>,
    ) -> RequestResult {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return refused(DeskError::new(
                "invalid-argument",
                "a request needs a prompt",
            ));
        }
        let pointing = if with_context.unwrap_or(true) {
            serde_json::to_value(context::pointing(&crate::app().app_id, None).await)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        // G-D5: the surface the person was looking at, as they saw it.
        let viewing = match surface_id.filter(|s| !s.trim().is_empty()) {
            Some(id) if with_context.unwrap_or(true) => context::viewing(id.trim()).await,
            _ => Value::Null,
        };
        file(
            desk::ASK,
            prompt,
            desk::tagged(
                "pointing",
                json!({ "pointing": pointing, "viewing": viewing }),
            ),
            None,
        )
    }

    async fn diagnose(&self, call_id: String, prompt: Option<String>) -> RequestResult {
        let entry =
            match call::call_async("notebook-service_entry", json!({ "call_id": call_id })).await {
                Ok(entry) => entry,
                Err(e) => return refused(DeskError::new("unavailable", e.to_string())),
            };
        if entry["ok"] != json!(true) {
            return refused(DeskError::new(
                "not-found",
                entry["message"]
                    .as_str()
                    .unwrap_or("the notebook has no record of that call")
                    .to_string(),
            ));
        }
        let verb_name = entry["record"]["verb"].as_str().unwrap_or("").to_string();
        let verb = VerbDescriptor::find(&verb_name).map(|v| {
            let handle = VerbHandle::Linked(v);
            json!({
                "name": handle.name(),
                "description": handle.description(),
                "safety": safety_word(handle.safety().class),
                "input_schema": handle.input_schema().into_owned(),
            })
        });
        let outcome = entry["summary"]["outcome"]
            .as_str()
            .unwrap_or("?")
            .to_string();
        let prompt = prompt.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| {
            if outcome == "ok" {
                format!("Explain what `{verb_name}` did in this call and where its time went.")
            } else {
                format!("Why did `{verb_name}` answer {outcome}, and what would fix it?")
            }
        });
        file(
            desk::DIAGNOSE,
            &prompt,
            desk::tagged(
                "call",
                json!({
                    "summary": entry["summary"],
                    "record": entry["record"],
                    "steps": entry["steps"],
                    "verb": verb,
                }),
            ),
            entry["call_id"].as_str(),
        )
    }

    async fn make_surface(&self, description: String, name: Option<String>) -> RequestResult {
        let description = description.trim();
        if description.is_empty() {
            return refused(DeskError::new(
                "invalid-argument",
                "say what the surface should show",
            ));
        }
        let kinds: Vec<String> = if impress_pane_query::process_manifest::is_installed() {
            impress_pane_query::process_manifest::current()
                .kinds
                .keys()
                .map(|k| k.to_string())
                .collect()
        } else {
            Vec::new()
        };
        let mut verbs: Vec<VerbRow> = call::descriptors().map(|v| verb_row(&v)).collect();
        verbs.sort_by(|a, b| a.name.cmp(&b.name));
        verbs.dedup_by(|a, b| a.name == b.name);
        let example: Value = serde_json::from_str(EXAMPLE_SPEC).unwrap_or(Value::Null);
        let prompt = match name.as_deref().filter(|n| !n.trim().is_empty()) {
            Some(name) => format!("Make a surface named \"{}\": {description}", name.trim()),
            None => format!("Make a surface: {description}"),
        };
        file(
            desk::MAKE_SURFACE,
            &prompt,
            desk::tagged(
                "build",
                json!({
                    "name": name,
                    "kinds": kinds,
                    "verbs": verbs,
                    "example_spec": example,
                    "how": [
                        "write the spec: state, sources (a verb call or a query), root nodes",
                        "impress-surface-service_surface-validate {spec}",
                        "impress-surface-service_surface-create {spec, name}",
                        "agent-service_answer {id, text, surface_id}: the desk offers Show",
                    ],
                }),
            ),
            None,
        )
    }

    async fn requests(&self, status: Option<String>) -> RequestsResult {
        let status = status.filter(|s| !s.trim().is_empty() && s.trim() != "all");
        let all = match desk::list(None) {
            Ok(all) => all,
            Err(e) => {
                return RequestsResult {
                    ok: false,
                    code: Some(e.code.into()),
                    message: Some(e.message),
                    ..Default::default()
                }
            }
        };
        let rows: Vec<RequestRow> = all.iter().map(|i| desk::row_of(i, false)).collect();
        RequestsResult {
            ok: true,
            open: rows.iter().filter(|r| r.status == "open").count() as u64,
            rows: rows
                .into_iter()
                .filter(|r| status.as_deref().is_none_or(|s| r.status == s.trim()))
                .collect(),
            ..Default::default()
        }
    }

    async fn next(&self, timeout_ms: Option<u64>) -> RequestResult {
        let deadline = std::time::Instant::now()
            + Duration::from_millis(timeout_ms.unwrap_or(0).min(MAX_WAIT_MS));
        loop {
            match desk::oldest_open() {
                Ok(Some(row)) => {
                    return RequestResult {
                        ok: true,
                        request: Some(row),
                        ..Default::default()
                    }
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok(None) => {
                    return RequestResult {
                        ok: true,
                        message: Some("no open request".into()),
                        ..Default::default()
                    }
                }
                Err(e) => return refused(e),
            }
        }
    }

    async fn claim(&self, id: String) -> RequestResult {
        filed(desk::transition(
            &id,
            &["open"],
            "claimed",
            vec![("claimed_by", json!(caller().author()))],
        ))
    }

    async fn answer(&self, id: String, text: String, surface_id: Option<String>) -> RequestResult {
        let mut fields = vec![
            ("answer", json!(text.trim())),
            ("answered_by", json!(caller().author())),
        ];
        if let Some(surface) = surface_id.filter(|s| !s.trim().is_empty()) {
            fields.push(("surface_id", json!(surface.trim())));
        }
        filed(desk::transition(
            &id,
            &["open", "claimed"],
            "answered",
            fields,
        ))
    }

    async fn decline(&self, id: String, reason: String) -> RequestResult {
        filed(desk::transition(
            &id,
            &["open", "claimed"],
            "declined",
            vec![
                ("answer", json!(reason.trim())),
                ("answered_by", json!(caller().author())),
            ],
        ))
    }

    async fn show(&self, id: String) -> ShowResult {
        let refuse = |e: DeskError| ShowResult {
            ok: false,
            code: Some(e.code.into()),
            message: Some(e.message),
            ..Default::default()
        };
        let item = match desk::get(&id) {
            Ok(item) => item,
            Err(e) => return refuse(e),
        };
        let Some(surface) = desk::row_of(&item, false).surface_id else {
            return refuse(DeskError::new(
                "invalid-argument",
                format!("request {id} names no surface"),
            ));
        };
        match call::call_async(
            "impress-surface-service_surface-show",
            json!({
                "id": surface,
                "target": {"split": {"direction": "horizontal"}},
                "app_id": crate::app().app_id,
            }),
        )
        .await
        {
            Ok(shown) => ShowResult {
                ok: shown["ok"] != json!(false),
                code: shown["code"].as_str().map(str::to_string),
                message: shown["message"].as_str().map(str::to_string),
                shown,
            },
            Err(e) => refuse(DeskError::new("unavailable", e.to_string())),
        }
    }
}

impress_service_impl! {
    service = AgentService,
    safety = read_only,
    since = "0.2.0",
    effects = {
        reads: [any("koine/agent-request@1.0.0")],
        writes: [],
        reach: [],
    },
    impl = DefaultAgentService,
    instance = DefaultAgentService::new,
    methods = [
        /// This app's skill, generated from the linked inventory.
        skill() -> SkillResult,
        /// What the person is pointing at.
        context(
            /// The app whose layout to read; this host's when absent.
            app_id: Option<String>,
            /// The device; this machine when absent.
            device: Option<String>
        ) -> ContextResult,
        /// Leave a request for an agent.
        ask(
            /// The request, in the person's words.
            prompt: String,
            /// Attach what the person is pointing at (default true).
            with_context: Option<bool>,
            /// The surface the person was looking at, attached as context.
            surface_id: Option<String>
        ) -> RequestResult,
        /// Hand a logged call to an agent.
        diagnose(
            /// The call's id (a notebook record).
            call_id: String,
            /// What to ask about it; a default question when absent.
            prompt: Option<String>
        ) -> RequestResult,
        /// Ask an agent for a surface.
        make_surface(
            /// What the surface should show and do.
            description: String,
            /// Its name.
            name: Option<String>
        ) -> RequestResult,
        /// The desk, newest first.
        requests(
            /// Only requests with this status: open, claimed, answered, declined.
            status: Option<String>
        ) -> RequestsResult,
        /// Wait for an open request.
        next(
            /// How long to wait in ms (default 0, at most 30000).
            timeout_ms: Option<u64>
        ) -> RequestResult,
        /// Take an open request.
        claim(
            /// The request's id.
            id: String
        ) -> RequestResult,
        /// Resolve a request.
        answer(
            /// The request's id.
            id: String,
            /// The answer.
            text: String,
            /// A surface the answer made.
            surface_id: Option<String>
        ) -> RequestResult,
        /// Give a request back.
        decline(
            /// The request's id.
            id: String,
            /// Why.
            reason: String
        ) -> RequestResult,
        /// Show an answer's surface beside the focused pane.
        show(
            /// The request's id.
            id: String
        ) -> ShowResult,
    ],
}
