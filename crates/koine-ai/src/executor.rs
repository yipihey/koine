use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use impress_item::item::{Item, ItemId, Value as CoreValue};
use koine_tasks::{ExecutionOutcome, TaskError, TaskExecutor, TaskStoreApi};

use crate::agent_loop::{self, AdapterToolRunner, LoopError, LoopObserver, ToolRunner};
use crate::blob::BlobStore;
use crate::registry::{AiRegistry, ResolveTarget};
use crate::research::ResearchContextProvider;
use crate::store::{AiStore, PreparedTurn, INFERENCE_TASK_KIND, TITLE_SUGGESTION_TASK_KIND};
use crate::tools::ToolAdapter;
use crate::types::{
    CompletionRecord, ModelContentPart, ModelMessage, Role, StreamEvent, ToolDefinition,
    ToolInvocationRecord,
};
use crate::{Error, InferenceProvider, OmlxClient};

const DEFAULT_MAX_TOOL_ROUNDS: u32 = 6;

/// Where an executor gets its client: one fixed provider (tests, single-host
/// deployments) or the device registry, which honours the conversation's
/// stored provider and the device selection per task.
enum Inference {
    Fixed(Arc<dyn InferenceProvider>),
    Registry(Arc<AiRegistry>),
}

impl Inference {
    /// The client plus the provider/endpoint identities recorded on the run.
    /// With a registry the prepared request's model may be filled in from
    /// the resolution when the conversation did not pin one.
    fn resolve(
        &self,
        prepared: &mut PreparedTurn,
    ) -> crate::Result<(Arc<dyn InferenceProvider>, String, String)> {
        match self {
            Self::Fixed(provider) => Ok((
                provider.clone(),
                provider.provider_id().to_string(),
                provider.endpoint_id().to_string(),
            )),
            Self::Registry(registry) => {
                let target = ResolveTarget {
                    provider: prepared.provider.clone(),
                    model: Some(prepared.request.model.clone())
                        .filter(|model| !model.trim().is_empty()),
                    category: None,
                };
                let resolved = registry.resolve(&target)?;
                prepared.request.model = resolved.model.clone();
                let client = registry.provider(&resolved.provider)?;
                Ok((client, resolved.provider, resolved.endpoint_id))
            }
        }
    }

    /// Reachability preflight for the scheduler's per-kind readiness gate:
    /// probe the client this executor would use for a DEFAULT task —
    /// the fixed provider, or the registry's device-default resolution.
    /// `models()` is the cheapest authenticated round-trip the provider
    /// trait offers. Per-conversation pins to OTHER providers share the
    /// verdict (a coarse gate beats acquiring provably-doomed work; the
    /// old behavior burned the whole retry budget per task in ~15s).
    async fn readiness_probe(&self) -> std::result::Result<(), String> {
        let (client, label) = match self {
            Self::Fixed(provider) => (provider.clone(), provider.endpoint_id().to_string()),
            Self::Registry(registry) => {
                let resolved = registry
                    .resolve(&ResolveTarget::default())
                    .map_err(|error| format!("registry resolution failed: {error}"))?;
                let client = registry
                    .provider(&resolved.provider)
                    .map_err(|error| format!("{}: {error}", resolved.provider))?;
                (client, resolved.endpoint_id)
            }
        };
        match tokio::time::timeout(Duration::from_secs(3), client.models()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(format!("provider {label} unreachable: {error}")),
            Err(_) => Err(format!("provider {label} probe timed out")),
        }
    }
}

/// Scheduler adapter that turns durable `impress.ai.respond` tasks into model
/// runs and appends the attributed assistant message back to the shared graph.
///
/// Tool schemas come from an installed [`ToolAdapter`]. Each model call is
/// executed, recorded, appended to the endpoint transcript, and followed by a
/// new model round. Only the final answer becomes a durable chat message.
pub struct AiTaskExecutor {
    ai: Arc<AiStore>,
    inference: Inference,
    blobs: Arc<dyn BlobStore>,
    tool_catalog: BTreeMap<String, Vec<ToolDefinition>>,
    tool_adapter: Option<Arc<dyn ToolAdapter>>,
    research: Option<Arc<dyn ResearchContextProvider>>,
    max_tool_rounds: u32,
}

impl AiTaskExecutor {
    pub fn new(ai: Arc<AiStore>, omlx: OmlxClient, blobs: Arc<dyn BlobStore>) -> Self {
        Self::with_provider(ai, Arc::new(omlx), blobs)
    }

    pub fn with_provider(
        ai: Arc<AiStore>,
        provider: Arc<dyn InferenceProvider>,
        blobs: Arc<dyn BlobStore>,
    ) -> Self {
        Self::with_inference(ai, Inference::Fixed(provider), blobs)
    }

    /// Resolve the client per task from the device registry (ADR-0029).
    pub fn with_registry(
        ai: Arc<AiStore>,
        registry: Arc<AiRegistry>,
        blobs: Arc<dyn BlobStore>,
    ) -> Self {
        Self::with_inference(ai, Inference::Registry(registry), blobs)
    }

    fn with_inference(ai: Arc<AiStore>, inference: Inference, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            ai,
            inference,
            blobs,
            tool_catalog: BTreeMap::new(),
            tool_adapter: None,
            research: None,
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
        }
    }

    /// Attach definitions without execution support. Kept for transport tests;
    /// production callers should install [`Self::with_tool_adapter`].
    pub fn with_tool_catalog(
        mut self,
        tool_catalog: BTreeMap<String, Vec<ToolDefinition>>,
    ) -> Self {
        self.tool_catalog = tool_catalog;
        self
    }

    pub fn with_tool_adapter(mut self, adapter: Arc<dyn ToolAdapter>) -> Self {
        self.tool_catalog = adapter.catalog();
        self.tool_adapter = Some(adapter);
        self
    }

    pub fn with_research_provider(mut self, provider: Arc<dyn ResearchContextProvider>) -> Self {
        self.research = Some(provider);
        self
    }

    pub fn with_max_tool_rounds(mut self, max_tool_rounds: u32) -> Self {
        self.max_tool_rounds = max_tool_rounds.clamp(1, 32);
        self
    }
}

/// Compatibility name for existing oMLX-only hosts. New hosts should prefer
/// [`AiTaskExecutor`] and [`AiTaskExecutor::with_provider`].
pub type OmlxTaskExecutor = AiTaskExecutor;

/// Dedicated provenance-bearing executor for conversation naming. Keeping
/// this as a separate durable task prevents title generation from delaying or
/// masquerading as an assistant response.
pub struct AiTitleTaskExecutor {
    ai: Arc<AiStore>,
    inference: Inference,
}

impl AiTitleTaskExecutor {
    pub fn new(ai: Arc<AiStore>, omlx: OmlxClient) -> Self {
        Self::with_provider(ai, Arc::new(omlx))
    }

    pub fn with_provider(ai: Arc<AiStore>, provider: Arc<dyn InferenceProvider>) -> Self {
        Self {
            ai,
            inference: Inference::Fixed(provider),
        }
    }

    pub fn with_registry(ai: Arc<AiStore>, registry: Arc<AiRegistry>) -> Self {
        Self {
            ai,
            inference: Inference::Registry(registry),
        }
    }
}

#[async_trait]
impl TaskExecutor for AiTaskExecutor {
    fn task_kind(&self) -> &str {
        INFERENCE_TASK_KIND
    }

    async fn execute(
        &self,
        task: &Item,
        _store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        if self.ai.has_completed_run(task.id).map_err(permanent)? {
            return Ok(ExecutionOutcome::Complete);
        }
        // LIVE catalog: with an adapter installed, re-derive per task so an
        // app that launched (or quit) after daemon start changes what the
        // model is shown. The construction-time snapshot froze the tool
        // surface for the daemon's whole life.
        let live_catalog = self.tool_adapter.as_ref().map(|adapter| adapter.catalog());
        let catalog = live_catalog.as_ref().unwrap_or(&self.tool_catalog);
        let mut prepared = self
            .ai
            .prepare_request(task.id, self.blobs.as_ref(), catalog)
            .map_err(permanent)?;
        let (client, provider_id, endpoint_id) = self
            .inference
            .resolve(&mut prepared)
            .map_err(classify_transport)?;

        let research_context = if prepared.request.tool_policy.allows("web") {
            if let Some(provider) = &self.research {
                let query = latest_user_text(&prepared.request.messages);
                let context = provider.gather(&query).await.map_err(classify_transport)?;
                if research_context_was_used(&context) {
                    self.ai
                        .attach_research_context(&mut prepared, self.blobs.as_ref(), &context)
                        .map_err(permanent)?;
                    Some((query, context))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let run_id = self
            .ai
            .record_run_start(task.id, &prepared, &provider_id, &endpoint_id)
            .map_err(permanent)?;
        if let Some((query, context)) = research_context {
            let mut result = BTreeMap::new();
            result.insert(
                "sources".into(),
                CoreValue::Array(
                    context
                        .sources
                        .iter()
                        .map(|source| CoreValue::String(source.url.clone()))
                        .collect(),
                ),
            );
            let error = if context.sources.is_empty() && !context.errors.is_empty() {
                Some(context.errors.join("; "))
            } else {
                None
            };
            self.ai
                .record_tool_invocation(
                    run_id,
                    task.id,
                    ToolInvocationRecord {
                        tool: "web.research-context".into(),
                        provider: "bounded-web".into(),
                        arguments: BTreeMap::from([("query".into(), CoreValue::String(query))]),
                        result: Some(result),
                        result_summary: Some(format!(
                            "{} source(s), {} retrieval warning(s)",
                            context.sources.len(),
                            context.errors.len()
                        )),
                        error,
                        duration_ms: None,
                    },
                )
                .map_err(permanent)?;
        }

        let started = Instant::now();
        let observer = RunObserver {
            ai: &self.ai,
            run_id,
            task_id: task.id,
            preview_published: AtomicBool::new(false),
        };
        let adapter_runner = self.tool_adapter.as_ref().map(|adapter| AdapterToolRunner {
            adapter: adapter.as_ref(),
            observer: &observer,
        });
        let runner = adapter_runner
            .as_ref()
            .map(|runner| runner as &dyn ToolRunner);
        let outcome = agent_loop::run(
            client.as_ref(),
            prepared.request.clone(),
            runner,
            &observer,
            self.max_tool_rounds,
        )
        .await
        .map_err(|error| match error {
            LoopError::Transport(error) => {
                let _ = self.ai.record_run_failure(run_id, &error.to_string());
                classify_transport(error)
            }
            LoopError::Stream(error) => {
                let _ = self.ai.record_run_failure(run_id, &error);
                TaskError::Retryable(error)
            }
            LoopError::Exhausted(error) => {
                let _ = self.ai.record_run_failure(run_id, &error);
                TaskError::Permanent(error)
            }
            LoopError::Invalid(error) | LoopError::Tool(error) => permanent(error),
        })?;
        let mut completion = CompletionRecord {
            content: outcome.content,
            reasoning: outcome.reasoning,
            usage: outcome.usage,
            finish_reason: outcome.finish_reason,
            ..CompletionRecord::default()
        };
        completion.duration_ms = Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        self.ai
            .record_completion(&prepared, run_id, completion)
            .map_err(permanent)?;
        // The response is already durable and visible. Naming is a distinct
        // task so it can run next without holding up this result.
        let _ = self
            .ai
            .queue_title_suggestion_if_placeholder(prepared.conversation_id);
        Ok(ExecutionOutcome::Complete)
    }

    /// Reachability preflight: the scheduler defers this whole kind while
    /// the provider is down instead of acquiring tasks that can only burn
    /// their retry budget against a connection refusal. `models()` is the
    /// cheapest authenticated round-trip the provider trait offers.
    async fn readiness(&self) -> std::result::Result<(), String> {
        self.inference.readiness_probe().await
    }

    // With backoff (45s·3^n) this covers ~30 minutes of provider outage
    // before escalating, instead of the old ~15 seconds.
    fn max_retries(&self) -> u32 {
        4
    }
}

#[async_trait]
impl TaskExecutor for AiTitleTaskExecutor {
    fn task_kind(&self) -> &str {
        TITLE_SUGGESTION_TASK_KIND
    }

    async fn execute(
        &self,
        task: &Item,
        _store: &dyn TaskStoreApi,
    ) -> Result<ExecutionOutcome, TaskError> {
        if self.ai.has_completed_run(task.id).map_err(permanent)? {
            return Ok(ExecutionOutcome::Complete);
        }
        let mut prepared = self.ai.prepare_title_request(task.id).map_err(permanent)?;
        let (client, provider_id, endpoint_id) = self
            .inference
            .resolve(&mut prepared)
            .map_err(classify_transport)?;
        let run_id = self
            .ai
            .record_run_start(task.id, &prepared, &provider_id, &endpoint_id)
            .map_err(permanent)?;
        let started = Instant::now();
        let mut stream = match client.stream(prepared.request.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                let _ = self.ai.record_run_failure(run_id, &error.to_string());
                return Err(classify_transport(error));
            }
        };
        let mut completion = CompletionRecord::default();
        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    let _ = self.ai.record_run_failure(run_id, &error.to_string());
                    return Err(classify_transport(error));
                }
            };
            match event {
                StreamEvent::Token { text } => completion.content.push_str(&text),
                StreamEvent::Reasoning { text } => completion.reasoning.push_str(&text),
                StreamEvent::Usage { usage } => completion.usage = Some(usage),
                StreamEvent::Done { finish_reason } => completion.finish_reason = finish_reason,
                StreamEvent::Error { error } => {
                    let _ = self.ai.record_run_failure(run_id, &error);
                    return Err(TaskError::Retryable(error));
                }
                StreamEvent::ToolCallDelta { .. } => {
                    let error = "title model unexpectedly requested a tool";
                    let _ = self.ai.record_run_failure(run_id, error);
                    return Err(TaskError::Permanent(error.into()));
                }
            }
        }
        let title = suggested_title(&completion.content).map_err(|error| {
            let _ = self.ai.record_run_failure(run_id, &error.to_string());
            permanent(error)
        })?;
        completion.duration_ms = Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        self.ai
            .record_title_completion(task.id, &prepared, run_id, title, completion)
            .map_err(permanent)?;
        Ok(ExecutionOutcome::Complete)
    }

    async fn readiness(&self) -> std::result::Result<(), String> {
        self.inference.readiness_probe().await
    }

    fn max_retries(&self) -> u32 {
        4
    }
}

fn latest_user_text(messages: &[ModelMessage]) -> String {
    messages
        .iter()
        .rfind(|message| message.role == Role::User)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|part| match part {
                    ModelContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn research_context_was_used(context: &crate::ResearchContext) -> bool {
    !context.sources.is_empty() || !context.errors.is_empty()
}

fn suggested_title(content: &str) -> crate::Result<String> {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let without_label = first_line
        .strip_prefix("Title:")
        .or_else(|| first_line.strip_prefix("title:"))
        .unwrap_or(first_line);
    let title = without_label
        .trim()
        .trim_matches(|character| matches!(character, '"' | '\'' | '`' | '*' | '#' | ' '))
        .trim_end_matches(['.', '!', '?', ':', ';'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return Err(Error::Invalid(
            "the model returned an empty conversation title".into(),
        ));
    }
    Ok(title.chars().take(120).collect())
}

#[derive(Debug, PartialEq, Eq)]
struct ResponsePreview<'a> {
    text: &'a str,
    paragraph_complete: bool,
}

/// Select the earliest coherent block worth publishing. Paragraph boundaries
/// win; a complete sentence provides a bounded fallback for models that emit
/// one very long paragraph. Incomplete TeX and code fences are never exposed.
fn response_preview(content: &str) -> Option<ResponsePreview<'_>> {
    for (boundary, _) in content.match_indices("\n\n") {
        let candidate = content[..boundary].trim_end();
        if has_substantive_final_block(candidate) && preview_is_balanced(candidate) {
            return Some(ResponsePreview {
                text: candidate,
                paragraph_complete: true,
            });
        }
    }

    if content.chars().count() < 240 {
        return None;
    }
    for (index, character) in content.char_indices() {
        if index < 120 || !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let end = index + character.len_utf8();
        if content[end..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            let candidate = content[..end].trim_end();
            if has_substantive_final_block(candidate) && preview_is_balanced(candidate) {
                return Some(ResponsePreview {
                    text: candidate,
                    paragraph_complete: false,
                });
            }
        }
    }

    if content.chars().count() < 480 {
        return None;
    }
    let end = content
        .char_indices()
        .take_while(|(index, _)| *index <= 480)
        .filter_map(|(index, character)| character.is_whitespace().then_some(index))
        .last()?;
    let candidate = content[..end].trim_end();
    (has_substantive_final_block(candidate) && preview_is_balanced(candidate)).then_some(
        ResponsePreview {
            text: candidate,
            paragraph_complete: false,
        },
    )
}

fn has_substantive_final_block(candidate: &str) -> bool {
    let block = candidate.rsplit("\n\n").next().unwrap_or(candidate).trim();
    !block.is_empty()
        && !block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.trim_start().starts_with('#'))
}

fn preview_is_balanced(candidate: &str) -> bool {
    if !candidate.match_indices("```").count().is_multiple_of(2) {
        return false;
    }
    let mut dollars = 0_u32;
    let mut escaped = false;
    for character in candidate.chars() {
        if character == '$' && !escaped {
            dollars += 1;
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    dollars.is_multiple_of(2)
        && candidate.matches("\\(").count() == candidate.matches("\\)").count()
        && candidate.matches("\\[").count() == candidate.matches("\\]").count()
}

/// Keeps the run record current while the loop runs: the first preview and
/// each tool invocation.
struct RunObserver<'a> {
    ai: &'a AiStore,
    run_id: ItemId,
    task_id: ItemId,
    preview_published: AtomicBool,
}

impl LoopObserver for RunObserver<'_> {
    fn on_text(&self, round_text: &str) -> crate::Result<()> {
        if self.preview_published.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(preview) = response_preview(round_text) {
            self.ai
                .record_run_preview(self.run_id, preview.text, preview.paragraph_complete)?;
            self.preview_published.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn on_tool_invocation(&self, record: ToolInvocationRecord) -> crate::Result<()> {
        self.ai
            .record_tool_invocation(self.run_id, self.task_id, record)
            .map(|_| ())
    }
}

fn permanent(error: Error) -> TaskError {
    TaskError::Permanent(error.to_string())
}

fn classify_transport(error: Error) -> TaskError {
    if error.is_retryable() {
        TaskError::Retryable(error.to_string())
    } else {
        TaskError::Permanent(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::{
        ConversationDraft, EventStream, FileBlobStore, InferenceProvider, MessageDraft,
        ModelSummary, Result as AiResult, ToolPolicy,
    };
    use impress_item::item::ActorKind;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    struct ScriptedProvider {
        rounds: Mutex<VecDeque<Vec<StreamEvent>>>,
        requests: Mutex<Vec<crate::ChatRequest>>,
    }

    #[async_trait]
    impl InferenceProvider for ScriptedProvider {
        fn provider_id(&self) -> &str {
            "scripted"
        }

        fn endpoint_id(&self) -> &str {
            "executor-test"
        }

        async fn models(&self) -> AiResult<Vec<ModelSummary>> {
            Ok(vec![])
        }

        async fn stream(&self, request: crate::ChatRequest) -> AiResult<EventStream> {
            self.requests.lock().unwrap().push(request);
            let events = self.rounds.lock().unwrap().pop_front().unwrap();
            let (sender, receiver) = mpsc::channel(events.len().max(1));
            for event in events {
                sender.try_send(Ok(event)).unwrap();
            }
            drop(sender);
            Ok(ReceiverStream::new(receiver))
        }
    }

    struct ScriptedTools;

    #[async_trait]
    impl ToolAdapter for ScriptedTools {
        fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>> {
            BTreeMap::from([(
                "scix".into(),
                vec![ToolDefinition {
                    name: "scix".into(),
                    description: "Search the literature".into(),
                    input_schema: json!({ "type": "object" }),
                }],
            )])
        }

        fn provider_id(&self, _tool_name: &str) -> String {
            "scripted-tools".into()
        }

        async fn call(&self, tool_name: &str, arguments: Value) -> AiResult<Value> {
            assert_eq!(tool_name, "scix");
            assert_eq!(arguments["query"], "stars");
            Ok(json!({ "papers": 3 }))
        }
    }

    #[test]
    fn registers_under_the_dispatchable_task_kind() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let blobs = Arc::new(FileBlobStore::open(directory.path().join("blobs")).unwrap());
        let executor = OmlxTaskExecutor::new(
            ai,
            OmlxClient::new("http://127.0.0.1:8000", None).unwrap(),
            blobs,
        );
        assert_eq!(executor.task_kind(), INFERENCE_TASK_KIND);
        // 4 retries × the scheduler's exponential backoff ≈ 30 minutes of
        // provider-outage coverage before escalation (was 2 ≈ 15 seconds).
        assert_eq!(executor.max_retries(), 4);
    }

    #[test]
    fn extracts_only_text_from_the_latest_user_turn() {
        let messages = vec![
            ModelMessage::text(Role::User, "old"),
            ModelMessage::text(Role::Assistant, "answer"),
            ModelMessage::text(Role::User, "new"),
        ];
        assert_eq!(latest_user_text(&messages), "new");
    }

    #[test]
    fn empty_research_policy_check_is_not_a_tool_use() {
        assert!(!research_context_was_used(
            &crate::ResearchContext::default()
        ));
        assert!(research_context_was_used(&crate::ResearchContext {
            sources: vec![],
            errors: vec!["search failed".into()],
        }));
    }

    #[test]
    fn cleans_common_model_title_wrappers() {
        assert_eq!(
            suggested_title("Title: **Galaxy Rotation Curves.**\nMore text").unwrap(),
            "Galaxy Rotation Curves"
        );
        assert!(suggested_title(" \n ").is_err());
    }

    #[test]
    fn preview_waits_for_prose_after_a_heading() {
        let text = "# Result\n\nThe first useful paragraph has arrived.\n\nMore follows.";
        assert_eq!(
            response_preview(text),
            Some(ResponsePreview {
                text: "# Result\n\nThe first useful paragraph has arrived.",
                paragraph_complete: true,
            })
        );
    }

    #[test]
    fn preview_uses_a_unicode_safe_sentence_fallback() {
        let sentence = format!(
            "{} This is a complete result sentence. trailing generation",
            "αβγδ ".repeat(40)
        );
        let preview = response_preview(&sentence).unwrap();
        assert!(preview.text.ends_with("sentence."));
        assert!(!preview.paragraph_complete);
        assert!(std::str::from_utf8(preview.text.as_bytes()).is_ok());
    }

    #[test]
    fn preview_never_exposes_unfinished_math_or_code() {
        let math = format!("{} The value is $x + y.\n\n", "context ".repeat(35));
        assert_eq!(response_preview(&math), None);
        let code = format!("{}\n```rust\nlet x = 1;\n\n", "context ".repeat(35));
        assert_eq!(response_preview(&code), None);
    }

    #[tokio::test]
    async fn provider_neutral_tool_loop_records_complete_lineage() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:executor",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let blobs = Arc::new(FileBlobStore::open(directory.path().join("blobs")).unwrap());
        let conversation = ai
            .create_conversation(ConversationDraft {
                model: "scripted-model".into(),
                tool_policy: ToolPolicy {
                    enabled: vec!["scix".into()],
                },
                ..Default::default()
            })
            .unwrap();
        let queued = ai
            .queue_user_turn(conversation, MessageDraft::user("Find papers"))
            .unwrap();
        let provider = Arc::new(ScriptedProvider {
            rounds: Mutex::new(VecDeque::from([
                vec![
                    StreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call-1".into()),
                        name: Some("scix".into()),
                        arguments: r#"{"query":"stars"}"#.into(),
                    },
                    StreamEvent::Done {
                        finish_reason: Some("tool_calls".into()),
                    },
                ],
                vec![
                    StreamEvent::Token {
                        text: "Three papers are relevant.".into(),
                    },
                    StreamEvent::Done {
                        finish_reason: Some("stop".into()),
                    },
                ],
            ])),
            requests: Mutex::new(vec![]),
        });
        let executor = AiTaskExecutor::with_provider(ai.clone(), provider.clone(), blobs)
            .with_tool_adapter(Arc::new(ScriptedTools));
        let task = ai.shared_store().get(queued.task_id).unwrap().unwrap();

        assert_eq!(
            executor
                .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
                .await
                .unwrap(),
            ExecutionOutcome::Complete
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1]
            .messages
            .iter()
            .any(|message| message.role == Role::Tool));
        drop(requests);

        let provenance = ai.task_provenance(queued.task_id).unwrap().unwrap();
        assert_eq!(
            provenance.run.payload["provider"],
            CoreValue::String("scripted".into())
        );
        assert_eq!(
            provenance.run.payload["endpoint"],
            CoreValue::String("executor-test".into())
        );
        assert_eq!(provenance.tool_invocations.len(), 1);
        assert_eq!(provenance.outputs.len(), 1);
    }

    #[tokio::test]
    async fn tool_limit_forces_a_final_tools_disabled_response() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:tool-limit",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let blobs = Arc::new(FileBlobStore::open(directory.path().join("blobs")).unwrap());
        let conversation = ai
            .create_conversation(ConversationDraft {
                model: "scripted-model".into(),
                tool_policy: ToolPolicy {
                    enabled: vec!["scix".into()],
                },
                ..Default::default()
            })
            .unwrap();
        let queued = ai
            .queue_user_turn(conversation, MessageDraft::user("Find papers"))
            .unwrap();
        let provider = Arc::new(ScriptedProvider {
            rounds: Mutex::new(VecDeque::from([
                vec![
                    StreamEvent::ToolCallDelta {
                        index: 0,
                        id: Some("call-1".into()),
                        name: Some("scix".into()),
                        arguments: r#"{"query":"stars"}"#.into(),
                    },
                    StreamEvent::Done {
                        finish_reason: Some("tool_calls".into()),
                    },
                ],
                vec![
                    StreamEvent::Token {
                        text: "Three papers are relevant.".into(),
                    },
                    StreamEvent::Done {
                        finish_reason: Some("stop".into()),
                    },
                ],
            ])),
            requests: Mutex::new(vec![]),
        });
        let executor = AiTaskExecutor::with_provider(ai.clone(), provider.clone(), blobs)
            .with_tool_adapter(Arc::new(ScriptedTools))
            .with_max_tool_rounds(1);
        let task = ai.shared_store().get(queued.task_id).unwrap().unwrap();

        assert_eq!(
            executor
                .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
                .await
                .unwrap(),
            ExecutionOutcome::Complete
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty());
        assert!(requests[1].messages.iter().any(|message| {
            message.role == Role::System
                && message.content.iter().any(|part| {
                    matches!(part, ModelContentPart::Text { text } if text.contains("Answer the user now"))
                })
        }));
    }

    #[tokio::test]
    async fn registry_executor_resolves_the_conversation_provider_per_task() {
        use crate::credentials::InMemoryCredentials;
        use mockito::Matcher;

        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:registry-executor",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let blobs = Arc::new(FileBlobStore::open(directory.path().join("blobs")).unwrap());
        let mut server = mockito::Server::new_async().await;
        let chat = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({ "model": "mlx-community--Qwen3.5-4B-4bit", "stream": true })))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Three papers.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ))
            .create_async()
            .await;
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        registry
            .set_provider_endpoint("omlx", Some(server.url()))
            .unwrap();

        let conversation = ai
            .create_conversation(ConversationDraft {
                provider: "omlx".into(),
                model: "mlx-community--Qwen3.5-4B-4bit".into(),
                ..Default::default()
            })
            .unwrap();
        let queued = ai
            .queue_user_turn(conversation, MessageDraft::user("Find papers"))
            .unwrap();
        let executor = AiTaskExecutor::with_registry(ai.clone(), registry, blobs);
        let task = ai.shared_store().get(queued.task_id).unwrap().unwrap();
        assert_eq!(
            executor
                .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
                .await
                .unwrap(),
            ExecutionOutcome::Complete
        );
        chat.assert_async().await;
        let provenance = ai.task_provenance(queued.task_id).unwrap().unwrap();
        assert_eq!(
            provenance.run.payload["provider"],
            CoreValue::String("omlx".into())
        );
        assert_ne!(
            provenance.run.payload["endpoint"],
            CoreValue::String("local-omlx".into()),
            "a custom endpoint is recorded under its own identity"
        );
        assert_eq!(provenance.outputs.len(), 1);
    }

    #[tokio::test]
    async fn registry_executor_fails_permanently_when_nothing_is_configured() {
        use crate::credentials::InMemoryCredentials;

        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:registry-unconfigured",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let blobs = Arc::new(FileBlobStore::open(directory.path().join("blobs")).unwrap());
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        let conversation = ai
            .create_conversation(ConversationDraft {
                provider: "anthropic".into(),
                model: "claude-opus-5".into(),
                ..Default::default()
            })
            .unwrap();
        let queued = ai
            .queue_user_turn(conversation, MessageDraft::user("hi"))
            .unwrap();
        let executor = AiTaskExecutor::with_registry(ai.clone(), registry, blobs);
        let task = ai.shared_store().get(queued.task_id).unwrap().unwrap();
        let error = executor
            .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
            .await
            .unwrap_err();
        assert!(
            matches!(error, TaskError::Permanent(ref message) if message.contains("not configured")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn title_executor_uses_local_model_and_updates_only_the_title() {
        let directory = tempfile::tempdir().unwrap();
        let ai = Arc::new(
            AiStore::open_dir(
                &directory.path().join("impress.sqlite"),
                "test:titles",
                ActorKind::Agent,
            )
            .unwrap(),
        );
        let conversation = ai
            .create_conversation(ConversationDraft {
                model: "scripted-model".into(),
                ..Default::default()
            })
            .unwrap();
        ai.append_message(
            conversation,
            MessageDraft::user("Explain the origin of galaxy rotation curves"),
        )
        .unwrap();
        let task_id = ai.queue_title_suggestion(conversation).unwrap();
        let provider = Arc::new(ScriptedProvider {
            rounds: Mutex::new(VecDeque::from([vec![
                StreamEvent::Token {
                    text: "Title: Galaxy Rotation Curve Origins".into(),
                },
                StreamEvent::Done {
                    finish_reason: Some("stop".into()),
                },
            ]])),
            requests: Mutex::new(vec![]),
        });
        let executor = AiTitleTaskExecutor::with_provider(ai.clone(), provider.clone());
        let task = ai.shared_store().get(task_id).unwrap().unwrap();

        assert_eq!(
            executor
                .execute(&task, &koine_tasks::ItemTaskStore::new(ai.shared_store()))
                .await
                .unwrap(),
            ExecutionOutcome::Complete
        );
        let view = ai.conversation_view(conversation).unwrap();
        assert_eq!(view.conversation.title, "Galaxy Rotation Curve Origins");
        assert_eq!(
            view.messages.len(),
            1,
            "title generation is not a chat turn"
        );
        assert!(ai.has_completed_run(task_id).unwrap());
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "scripted-model");
        assert!(requests[0].tools.is_empty());
        assert!(!requests[0].thinking);
    }
}
