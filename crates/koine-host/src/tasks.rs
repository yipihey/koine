//! A host runs tasks (docs/plan-koine-ai-tasks.md Q-D5) and, with AI, the
//! desk agent (AI-D5).
//!
//! [`start`] runs `koine-tasks`' scheduler over the process store: a pass
//! every `poll`, after `start_delay`. It registers the verb executor (Q-D3),
//! so `task-service_enqueue` work runs here as `agent:tasks`. With AI it
//! also registers the model executors (a queued conversation turn, a title
//! suggestion) over the same store and `<workspace>/ai/`. It then runs the
//! desk agent: open requests on the agent desk become conversations with the
//! linked inventory as tools (`koine-verbs`), and their replies become the
//! answers.
//!
//! The scheduler's actor (`<app_id>:tasks`) is what it stamps as
//! `assigned_to`, so a restarted host resumes its own running tasks. One
//! host per store runs a scheduler. Two would resume each other's tasks.

use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use impress_item::AttributedStore;
use impress_service_core::async_trait::async_trait;
use koine_ai::schema::Profile;
use koine_ai::{
    AiRegistry, AiStore, FileBlobStore, InventoryTools, ResolveTarget, ToolAdapter, ToolDefinition,
};
use koine_tasks::{ItemTaskStore, PassReport, Scheduler, SchedulerConfig, VerbTaskExecutor};
use tokio::task::JoinHandle;

/// The agent verb tasks run as.
pub const VERB_AGENT: &str = "tasks";
/// The agent the desk bridge and the model's tool calls run as.
pub const AI_AGENT: &str = "koine-ai";
/// How often the desk is read.
pub const DESK_POLL: Duration = Duration::from_secs(3);

/// The verbs the on-device Apple model may call unless the host names others
/// (`--native-tools`), most useful first: read-only ones that answer
/// questions about what the host holds. Its window is about 4k tokens, so
/// the set is small and [`NATIVE_TOOLS_MAX_CHARS`] caps it further.
pub const DEFAULT_NATIVE_TOOLS: &[&str] = &[
    "notebook-service_timeline",
    // What the person and earlier sessions taught it (KM-D6).
    "memory-service_recall",
    "task-service_list",
    "notebook-service_entry",
    "impress-surface-service_surface-list",
    "agent-service_requests",
    "package-service_list",
    "task-service_show",
    "notebook-service_runs",
];
/// The most characters of tool definitions (name, description and input
/// schema) the on-device model is shown: roughly 1.5k tokens of its window.
pub const NATIVE_TOOLS_MAX_CHARS: usize = 6_000;

/// How many characters of `tool` the provider sends the model: its name,
/// description and input schema (an output schema stays on this side).
fn sent_size(tool: &ToolDefinition) -> usize {
    tool.name.len() + tool.description.len() + tool.input_schema.to_string().len()
}

/// The flat, Apple-lowered catalog for a provider that runs its own tool
/// loop (LM-D11), cut to a size its context can hold: definitions are kept
/// in the allowlist's order until the serialized total would pass
/// `max_chars`, and the rest are neither shown nor callable.
pub struct CappedTools {
    inner: InventoryTools,
    allow: Vec<String>,
    max_chars: usize,
}

impl CappedTools {
    /// The allowed verbs as `agent`, lowered for Apple, without the model's
    /// own configuration verbs.
    pub fn apple(agent: &str, allow: Vec<String>, max_chars: usize) -> Self {
        Self {
            inner: InventoryTools::flat(agent, allow.clone(), Some(Profile::AppleGuided))
                .excluding(["impress-ai-service"]),
            allow,
            max_chars,
        }
    }

    fn rank(&self, name: &str) -> usize {
        self.allow
            .iter()
            .position(|entry| entry == name || (entry.ends_with('_') && name.starts_with(entry)))
            .unwrap_or(usize::MAX)
    }

    /// What the model is shown, and the names that did not fit.
    pub fn offered(&self) -> (Vec<ToolDefinition>, Vec<String>) {
        let mut all = self
            .inner
            .catalog()
            .into_values()
            .next()
            .unwrap_or_default();
        all.sort_by_key(|tool| (self.rank(&tool.name), tool.name.clone()));
        let (mut shown, mut used) = (Vec::new(), 0);
        let mut rest = all.into_iter();
        for tool in rest.by_ref() {
            let size = sent_size(&tool);
            if used + size > self.max_chars {
                let mut dropped = vec![tool.name];
                dropped.extend(rest.map(|tool| tool.name));
                return (shown, dropped);
            }
            used += size;
            shown.push(tool);
        }
        (shown, Vec::new())
    }
}

#[async_trait]
impl ToolAdapter for CappedTools {
    fn catalog(&self) -> BTreeMap<String, Vec<ToolDefinition>> {
        BTreeMap::from([(
            koine_ai::inventory_tools::POLICY_ID.to_string(),
            self.offered().0,
        )])
    }

    fn provider_id(&self, tool_name: &str) -> String {
        self.inner.provider_id(tool_name)
    }

    async fn call(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> koine_ai::Result<serde_json::Value> {
        if !self.offered().0.iter().any(|tool| tool.name == tool_name) {
            return Err(koine_ai::Error::Invalid(format!(
                "{tool_name}: not offered"
            )));
        }
        self.inner.call(tool_name, arguments).await
    }
}

/// What [`start`] runs.
#[derive(Clone, Debug)]
pub struct TaskRuntime {
    pub app_id: String,
    pub workspace: std::path::PathBuf,
    pub poll: Duration,
    pub start_delay: Duration,
    pub ai: bool,
    /// The verbs the on-device Apple model may call; `None` is
    /// [`DEFAULT_NATIVE_TOOLS`].
    pub native_tools: Option<Vec<String>>,
}

/// The background loops a host stops on shutdown.
pub struct Running {
    handles: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn stop(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

fn quiet(report: &PassReport) -> bool {
    *report == PassReport::default()
}

/// Start the scheduler (and with `runtime.ai` the model executors and the
/// desk agent) over `store`.
pub fn start(runtime: TaskRuntime, store: Arc<dyn AttributedStore>) -> Running {
    let mut scheduler = Scheduler::new(
        Arc::new(ItemTaskStore::new(store.clone())),
        SchedulerConfig {
            actor: format!("{}:tasks", runtime.app_id),
            start_delay: runtime.start_delay,
            poll_interval: runtime.poll,
            ..SchedulerConfig::default()
        },
    );
    scheduler.register(Arc::new(VerbTaskExecutor::new(VERB_AGENT)));
    let mut handles = Vec::new();

    if runtime.ai {
        let ai = Arc::new(AiStore::from_port(
            store.clone(),
            format!("agent:{}", AI_AGENT),
        ));
        let registry = AiRegistry::for_daemon(runtime.workspace.clone());
        #[cfg(feature = "apple-fm")]
        {
            let ready = koine_ai_apple::install(&registry);
            tracing::info!(target: "koine-host",
                "Apple on-device model: {}",
                if ready { "available" } else { "not available" });
        }
        let blob_root = runtime.workspace.join("ai").join("blobs");
        match FileBlobStore::open(&blob_root) {
            Ok(blobs) => {
                let tools = Arc::new(
                    InventoryTools::new(AI_AGENT)
                        // The model does not configure its own provider.
                        .excluding(["impress-ai-service"]),
                );
                #[allow(unused_mut)]
                let mut executor = koine_ai::AiTaskExecutor::with_registry(
                    ai.clone(),
                    registry.clone(),
                    Arc::new(blobs),
                )
                .with_tool_adapter(tools);
                // A provider that runs its own tool loop (Apple's) gets a
                // small flat catalog its window can hold, not the grouped one.
                #[cfg(feature = "apple-fm")]
                {
                    let allow = runtime.native_tools.clone().unwrap_or_else(|| {
                        DEFAULT_NATIVE_TOOLS.iter().map(|v| v.to_string()).collect()
                    });
                    let native =
                        CappedTools::apple(AI_AGENT, allow.clone(), NATIVE_TOOLS_MAX_CHARS);
                    let (shown, dropped) = native.offered();
                    tracing::info!(target: "koine-host",
                        "on-device tools: {}",
                        shown.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "));
                    if !dropped.is_empty() {
                        tracing::warn!(target: "koine-host",
                            "on-device tools over {NATIVE_TOOLS_MAX_CHARS} characters, not offered: {}",
                            dropped.join(", "));
                    }
                    executor = executor.with_native_tool_adapter(Arc::new(native));
                }
                scheduler.register(Arc::new(executor));
                scheduler.register(Arc::new(koine_ai::AiTitleTaskExecutor::with_registry(
                    ai.clone(),
                    registry.clone(),
                )));
                handles.push(tokio::spawn(desk_loop(ai, registry)));
            }
            Err(error) => tracing::warn!(target: "koine-host",
                "AI off: the blob store {} did not open: {error}", blob_root.display()),
        }
    }

    let poll = runtime.poll;
    let delay = runtime.start_delay;
    handles.push(tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        loop {
            match scheduler.run_once().await {
                Ok(report) if !quiet(&report) => {
                    tracing::info!(target: "koine-host", "tasks: {report:?}")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(target: "koine-host", "tasks: {error}"),
            }
            tokio::time::sleep(poll).await;
        }
    }));
    Running { handles }
}

/// Whether the desk has a model to ask: the device default resolves, or a
/// category the desk routes by (LM-D11) is assigned.
fn desk_can_resolve(registry: &AiRegistry) -> Result<(), koine_ai::Error> {
    let default = registry.resolve(&ResolveTarget::default());
    let routed = ["agent", "agent.frontier"].iter().any(|category| {
        registry
            .resolve(&ResolveTarget::category(*category))
            .is_ok()
    });
    match default {
        Err(error) if !routed => Err(error),
        _ => Ok(()),
    }
}

/// The desk agent: each pass answers what it can. Every request is resolved
/// on its own (its category, the `agent.route` triage, else the device's
/// model, so a choice made in settings is the one used next).
async fn desk_loop(ai: Arc<AiStore>, registry: Arc<AiRegistry>) {
    let mut said_off = false;
    loop {
        match desk_can_resolve(&registry) {
            Ok(()) => {
                said_off = false;
                let bridge = koine_ai::DeskBridge::with_registry(ai.clone(), registry.clone())
                    .as_agent(AI_AGENT);
                match bridge.pass().await {
                    Ok(pass) if pass != koine_ai::desk::Pass::default() => {
                        tracing::info!(target: "koine-host",
                            "desk: {} started, {} answered, {} declined",
                            pass.started, pass.answered, pass.declined)
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(target: "koine-host", "desk: {error}"),
                }
            }
            Err(error) => {
                if !said_off {
                    tracing::info!(target: "koine-host",
                        "desk agent waiting: no AI provider resolved ({error})");
                    said_off = true;
                }
            }
        }
        tokio::time::sleep(DESK_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_allow() -> Vec<String> {
        DEFAULT_NATIVE_TOOLS.iter().map(|v| v.to_string()).collect()
    }

    /// The on-device model's default tools are read-only verbs this host
    /// links, flat and lowered for Apple, and together they fit the cap its
    /// small window allows.
    #[test]
    fn the_default_native_tools_are_a_small_read_only_flat_catalog() {
        crate::ensure_linked();
        let tools = CappedTools::apple(AI_AGENT, default_allow(), NATIVE_TOOLS_MAX_CHARS);
        let (shown, dropped) = tools.offered();
        let names: Vec<&str> = shown.iter().map(|t| t.name.as_str()).collect();
        assert!(dropped.is_empty(), "over the cap: {dropped:?}");
        assert_eq!(
            names, DEFAULT_NATIVE_TOOLS,
            "all linked, in the allowlist's order"
        );
        for tool in &shown {
            assert_eq!(
                tool.annotations.as_ref().map(|a| a["readOnlyHint"].clone()),
                Some(serde_json::json!(true)),
                "{} is not read-only",
                tool.name
            );
            assert!(
                tool.input_schema.is_object(),
                "{} has no flat schema",
                tool.name
            );
        }
        let size: usize = shown.iter().map(sent_size).sum();
        println!("default native tools: {size} characters");
        assert!(size <= NATIVE_TOOLS_MAX_CHARS, "{size} characters");
    }

    /// Over the cap the later definitions are dropped, not the earlier ones,
    /// and a dropped verb cannot be called; a host's own list wins, and a
    /// namespace prefix works.
    #[tokio::test]
    async fn a_cap_drops_the_later_tools_and_they_are_not_callable() {
        crate::ensure_linked();
        let one = CappedTools::apple(AI_AGENT, default_allow(), usize::MAX)
            .offered()
            .0;
        let first = sent_size(&one[0]);
        let tools = CappedTools::apple(AI_AGENT, default_allow(), first + 10);
        let (shown, dropped) = tools.offered();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, DEFAULT_NATIVE_TOOLS[0]);
        assert_eq!(dropped.len(), DEFAULT_NATIVE_TOOLS.len() - 1);
        let refused = tools
            .call(DEFAULT_NATIVE_TOOLS[1], serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("not offered"), "{refused}");

        let own = CappedTools::apple(
            AI_AGENT,
            vec!["package-service_".into()],
            NATIVE_TOOLS_MAX_CHARS,
        );
        let names: Vec<String> = own.offered().0.into_iter().map(|t| t.name).collect();
        assert!(names.iter().all(|n| n.starts_with("package-service_")));
        assert!(names.contains(&"package-service_list".to_string()));
        assert!(
            CappedTools::apple(AI_AGENT, vec!["impress-ai-service_".into()], usize::MAX)
                .offered()
                .0
                .is_empty(),
            "the model does not configure its own provider"
        );
    }
}
