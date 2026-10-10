//! End-to-end demonstration of the impress service codegen pipeline.
//!
//! Run with `cargo run --example echo_demo -p impress-service-core`.
//!
//! What it shows:
//! * A trait annotated with `#[impress_service]`.
//! * A concrete impl registered via `impress_service_impl!`.
//! * The `McpToolDescriptor` inventory: tools discovered automatically.
//! * The `CliSubcommand` inventory: subcommands discovered automatically.
//! * Direct invocation of the generated async handler with a JSON args object.

use impress_service_core::{runtime, CliSubcommand, McpToolDescriptor};
use impress_service_macros::{impress_service, impress_service_impl};
// `impress_method` is referenced by attribute syntax inside the trait below;
// the marker attribute is consumed by `#[impress_service]`.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use serde_json::json;

#[impress_service]
pub trait EchoService: Send + Sync + 'static {
    /// Echo a message back to the caller.
    #[impress_method]
    async fn echo(&self, message: String) -> String;

    /// Repeat a message a specified number of times.
    #[impress_method]
    async fn repeat(&self, message: String, count: i64) -> Vec<String>;
}

pub struct DemoEcho;

#[async_trait::async_trait]
impl EchoService for DemoEcho {
    async fn echo(&self, message: String) -> String {
        format!("echo: {message}")
    }

    async fn repeat(&self, message: String, count: i64) -> Vec<String> {
        let n = count.max(0) as usize;
        std::iter::repeat_n(message, n).collect()
    }
}

impress_service_impl! {
    service = EchoService,
    impl = DemoEcho,
    instance = || DemoEcho,
    safety = read_only,
    since = "0.1.0",
    methods = [
        /// Echo a message back to the caller.
        echo(message: String) -> String,
        /// Repeat a message a specified number of times.
        repeat(message: String, count: i64) -> Vec<String>,
    ],
}

fn main() {
    println!("== MCP tool descriptors ==");
    let mut mcp_count = 0;
    for tool in McpToolDescriptor::iter() {
        mcp_count += 1;
        let schema = (tool.input_schema)();
        println!(
            "  - {}\n      description: {}\n      input_schema: {}",
            tool.name,
            tool.description,
            serde_json::to_string(&schema).unwrap_or_default()
        );
    }
    println!("  ({mcp_count} total)\n");

    println!("== CLI subcommands ==");
    let mut cli_count = 0;
    for sub in CliSubcommand::iter() {
        cli_count += 1;
        println!("  - {}: {}", sub.name, sub.description);
    }
    println!("  ({cli_count} total)\n");

    println!("== Invoking the `echoservice_echo` handler ==");
    let echo_tool = McpToolDescriptor::iter()
        .find(|t| t.name.ends_with("_echo"))
        .expect("echo tool should be registered");
    let result = impress_service_core::pipeline::invoke_blocking(
        echo_tool.verb,
        impress_service_core::pipeline::Call::agent("example", json!({ "message": "hi" })),
    )
    .expect("echo should succeed");
    println!("  result: {result}\n");

    println!("== Invoking the `echoservice_repeat` handler ==");
    let repeat_tool = McpToolDescriptor::iter()
        .find(|t| t.name.ends_with("_repeat"))
        .expect("repeat tool should be registered");
    let result = impress_service_core::pipeline::invoke_blocking(
        repeat_tool.verb,
        impress_service_core::pipeline::Call::agent(
            "example",
            json!({ "message": "ho", "count": 3 }),
        ),
    )
    .expect("repeat should succeed");
    println!("  result: {result}");

    assert!(mcp_count >= 2, "expected at least 2 MCP tools");
    assert!(cli_count >= 2, "expected at least 2 CLI subcommands");
}
