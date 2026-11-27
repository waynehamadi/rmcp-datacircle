//! Four MCP tools for [rmcp](https://github.com/modelcontextprotocol/rust-sdk), the official Rust MCP SDK, on
//! Datacircle's open API: find the best GTM APIs.
//!
//! We benchmark all the GTM APIs and we put the results on our website, <https://datacircle.dev>. The API needs no key
//! and no account: <https://api.datacircle.dev/v1> (its spec: <https://api.datacircle.dev/v1/openapi.json>).
//!
//! | Tool | Call | What it gives |
//! |---|---|---|
//! | `datacircle_list_apis` | `GET /apis` | every API on the sheet, its slug and its best plan |
//! | `datacircle_best_plans` | `GET /apis/{slug}/best` | an API's plans, best first |
//! | `datacircle_search_plans` | `GET /plans` | plans by API, provider or plan name, by score, name or price |
//! | `datacircle_provider` | `GET /providers/{slug}` | the APIs one provider sells and its rank on each |
//!
//! [`Datacircle::tool_router`] gives the four as routes of a server's [`ToolRouter`], beside its own tools. The server
//! keeps the two routers added in a field, which its `#[tool_handler]` names (without `router`, the macro serves
//! `Self::tool_router()`, the server's own tools alone):
//!
//! ```
//! use rmcp::{ServerHandler, handler::server::tool::ToolRouter, tool, tool_handler, tool_router};
//! use rmcp_datacircle::Datacircle;
//!
//! #[derive(Clone)]
//! struct App {
//!     tool_router: ToolRouter<Self>,
//! }
//!
//! #[tool_router]
//! impl App {
//!     fn new() -> Self {
//!         Self { tool_router: Self::tool_router() + Datacircle::new().tool_router() }
//!     }
//!
//!     #[tool(description = "Says hello")]
//!     fn hello(&self) -> String {
//!         "Hello".to_owned()
//!     }
//! }
//!
//! #[tool_handler(router = self.tool_router)]
//! impl ServerHandler for App {}
//!
//! assert_eq!(App::new().tool_router.list_all().len(), 5);
//! ```
//!
//! [`Datacircle`] is a [`ServerHandler`] of the four tools too, and the `datacircle-mcp` binary serves it over stdio.

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod client;
mod error;
mod logic;
mod tools;

pub use client::BASE_URL;
pub use error::Error;
pub use logic::{BestPlansArgs, ListApisArgs, ProviderArgs, SearchPlansArgs};

use std::{borrow::Cow, sync::Arc, time::Duration};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::tool::{ToolRoute, ToolRouter},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, InitializeResult, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
    },
    service::{MaybeSend, RequestContext},
};
use serde_json::Value;

/// The four tools' names, in the order [`Datacircle::tools`] gives them.
pub const NAMES: [&str; 4] = [
    tools::LIST_APIS,
    tools::BEST_PLANS,
    tools::SEARCH_PLANS,
    tools::PROVIDER,
];

const INSTRUCTIONS: &str = "Datacircle's tools find the best GTM APIs, from the benchmark at https://datacircle.dev: \
datacircle_list_apis gives each API's slug, datacircle_best_plans an API's best plans, datacircle_search_plans plans \
by provider, plan name or price, and datacircle_provider one provider's rank on each API.";

/// Datacircle's open API: its four tools, and its calls as they run them. Cheap to clone: its clones share one HTTP
/// client.
#[derive(Clone, Debug)]
pub struct Datacircle {
    client: Arc<client::Client>,
}

impl Default for Datacircle {
    fn default() -> Self {
        Self::new()
    }
}

impl Datacircle {
    /// Calls [`BASE_URL`], each call waiting 30 s at most.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Sets where the tools call, and how long a call may take.
    pub fn builder() -> DatacircleBuilder {
        DatacircleBuilder::default()
    }

    /// The four tools as routes of a server's [`ToolRouter`], whatever the server: add it to the server's own
    /// (`Self::tool_router() + Datacircle::new().tool_router()`) or give it alone to a server of its own.
    pub fn tool_router<S: MaybeSend + 'static>(&self) -> ToolRouter<S> {
        let mut router = ToolRouter::new();
        for tool in tools::definitions() {
            let client = Arc::clone(&self.client);
            let name = tool.name.clone();
            router.add_route(ToolRoute::new(tool, move |arguments: JsonObject| {
                let (client, name) = (Arc::clone(&client), name.clone());
                async move { tools::call(&client, &name, Some(arguments)).await }
            }));
        }
        router
    }

    /// The four tools' definitions, for a server whose own `list_tools` gives them beside its tools; its `call_tool`
    /// then gives a call of one of [`NAMES`] to [`Datacircle::call`].
    pub fn tools(&self) -> Vec<Tool> {
        tools::definitions()
    }

    /// Runs a call of the tool named `name`: its result as an MCP client reads it, the answer as structured content
    /// and its JSON text, or an error result (`isError`) whose text says what to send instead.
    pub async fn call(&self, name: &str, arguments: Option<JsonObject>) -> CallToolResult {
        tools::call(&self.client, name, arguments).await
    }

    /// `datacircle_list_apis`: the APIs on the sheet, those whose name holds the most of `query`'s words first.
    pub async fn list_apis(&self, args: ListApisArgs) -> Result<Value, Error> {
        logic::list_apis(&self.client, args).await
    }

    /// `datacircle_best_plans`: an API's plans, best first.
    pub async fn best_plans(&self, args: BestPlansArgs) -> Result<Value, Error> {
        logic::best_plans(&self.client, args).await
    }

    /// `datacircle_search_plans`: the plans of every API or one, by provider or plan name.
    pub async fn search_plans(&self, args: SearchPlansArgs) -> Result<Value, Error> {
        logic::search_plans(&self.client, args).await
    }

    /// `datacircle_provider`: the APIs one provider sells and its rank on each, its best rank first.
    pub async fn provider(&self, args: ProviderArgs) -> Result<Value, Error> {
        logic::provider(&self.client, args).await
    }
}

/// A server of the four tools alone, as `datacircle-mcp` serves it: `Datacircle::new().serve(rmcp::transport::stdio())`.
impl ServerHandler for Datacircle {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("datacircle", env!("CARGO_PKG_VERSION"))
                    .with_title("Datacircle")
                    .with_description(
                        "Find the best GTM APIs. We benchmark all the GTM APIs and we put the results on our website.",
                    )
                    .with_website_url("https://datacircle.dev"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !NAMES.contains(&request.name.as_ref()) {
            return Err(ErrorData::invalid_params(
                Cow::Owned(format!(
                    "No tool is named {:?}: the tools are {}",
                    request.name,
                    NAMES.join(", ")
                )),
                None,
            ));
        }
        Ok(self.call(&request.name, request.arguments).await.into())
    }
}

/// Builds a [`Datacircle`]: where it calls and how long a call may take.
#[derive(Debug, Default)]
pub struct DatacircleBuilder {
    base_url: Option<String>,
    timeout: Option<Duration>,
    http: Option<reqwest::Client>,
}

impl DatacircleBuilder {
    /// The API's address, [`BASE_URL`] when left out.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// How long one call may take, 30 s when left out. A 429 is waited for once on top of it, as its Retry-After
    /// says, a minute at most.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// An HTTP client of the app's own (its proxy, its TLS): its own timeout then holds, [`Self::timeout`] none.
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// The [`Datacircle`] these settings make.
    pub fn build(self) -> Datacircle {
        let base = self
            .base_url
            .unwrap_or_else(|| BASE_URL.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let http = self.http.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout.unwrap_or(Duration::from_secs(30)))
                .build()
                .unwrap_or_default()
        });
        Datacircle {
            client: Arc::new(client::Client::new(base, http)),
        }
    }
}
