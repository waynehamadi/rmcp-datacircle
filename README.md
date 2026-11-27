# rmcp-datacircle

Find the best GTM APIs. We benchmark all the GTM APIs and we put the results on our website: https://datacircle.dev

This Rust crate gives servers of [rmcp](https://github.com/modelcontextprotocol/rust-sdk), the official Rust MCP SDK, four tools that call Datacircle's open API, https://api.datacircle.dev/v1. It also comes with an MCP server of its own on stdio, `datacircle-mcp`. You need no key and no account.

| Tool | What the tool gives | Call |
| --- | --- | --- |
| `datacircle_list_apis` | Every GTM API on Datacircle, with the API's slug, number of plans, number of providers and best plan | `GET /apis` |
| `datacircle_best_plans` | One API's plans, sorted by score out of 100, best first. Each plan comes with a rank and a price per 1,000 calls | `GET /apis/{slug}/best` |
| `datacircle_search_plans` | Plans that match a provider or plan name, on all APIs or on one API. Sort by score, provider or price | `GET /plans` |
| `datacircle_provider` | The APIs a provider sells, and the provider's rank on each API | `GET /providers/{slug}` |

## Install

```sh
cargo add rmcp-datacircle --git https://github.com/waynehamadi/rmcp-datacircle --tag v0.1.0
```

It works with rmcp 3.0 and later, and Rust 1.88 or later.

## Add the tools to your server

`Datacircle::tool_router()` gives the four tools as a `ToolRouter`. Add it to your server's own router, and keep the sum in a field:

```rust,no_run
use rmcp::{ServerHandler, ServiceExt, handler::server::tool::ToolRouter, tool, tool_handler, tool_router};
use rmcp_datacircle::Datacircle;

#[derive(Clone)]
struct MyServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MyServer {
    fn new() -> Self {
        Self { tool_router: Self::tool_router() + Datacircle::new().tool_router() }
    }

    #[tool(description = "Says hello")]
    fn hello(&self) -> String {
        "Hello!".to_owned()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MyServer {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = MyServer::new().serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}
```

The server lists your tools and our four tools. Write `#[tool_handler(router = self.tool_router)]`, not `#[tool_handler]` alone. Alone, the macro serves `Self::tool_router()`, which holds only your own tools.

If you write your own `list_tools` and `call_tool`, add our tools to them. `Datacircle::tools()` gives the four tools' definitions. `Datacircle::call(name, arguments)` runs a call of one of them, and `rmcp_datacircle::NAMES` holds their names:

```rust,no_run
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp_datacircle::{Datacircle, NAMES};

async fn call_tool(datacircle: &Datacircle, request: CallToolRequestParams) -> CallToolResult {
    if NAMES.contains(&request.name.as_ref()) {
        return datacircle.call(&request.name, request.arguments).await;
    }
    // ...your own tools
    CallToolResult::success(vec![])
}

fn list_tools(datacircle: &Datacircle, mine: Vec<Tool>) -> Vec<Tool> {
    mine.into_iter().chain(datacircle.tools()).collect()
}
```

`Datacircle` is also a `ServerHandler` with the four tools and nothing else: `Datacircle::new().serve(rmcp::transport::stdio())`.

## The stdio server

```sh
cargo install --git https://github.com/waynehamadi/rmcp-datacircle --tag v0.1.0
```

This installs `datacircle-mcp`. Add it to any MCP client as a stdio server, with `datacircle-mcp` as its command. For example, in a client's JSON config:

```json
{
  "mcpServers": {
    "datacircle": { "command": "datacircle-mcp" }
  }
}
```

## What a tool gives

Each tool gives back a JSON object, as structured content and as JSON text. Each tool has an output schema for it. Each plan comes with its rank, provider, plan, score, price per 1,000 (`perThousand`, in dollars) and its page on datacircle.dev. That page lists every field. A tool also takes an API's or a provider's name where it asks for a slug.

When the API gives no data, like for a slug it doesn't know, the tool gives an error result (`isError`). Its text has the API's own words and what to send instead, so a client can call again with it. A network failure is given the same way. All four tools are read-only.

## Call a tool yourself

```rust,no_run
use rmcp_datacircle::{BestPlansArgs, Datacircle};

#[tokio::main]
async fn main() -> Result<(), rmcp_datacircle::Error> {
    let plans = Datacircle::new()
        .best_plans(BestPlansArgs { slug: Some("email-finder".into()), limit: Some(3) })
        .await?;
    println!("{plans}");
    Ok(())
}
```

## Settings

```rust,no_run
use std::time::Duration;
use rmcp_datacircle::Datacircle;

let datacircle = Datacircle::builder()
    .timeout(Duration::from_secs(10)) // 30 s when left out
    .build();
```

`.http_client(...)` takes a `reqwest::Client` of your own (a proxy, your TLS). The crate's calls use rustls. The `native-tls` feature uses your system's TLS instead. The `stdio` feature builds `datacircle-mcp`. To leave both out: `default-features = false, features = ["rustls"]`.

## Network

Each tool call makes one HTTPS GET request to https://api.datacircle.dev/v1, or a few when `datacircle_search_plans` tries a name again another way, and nothing else. The request holds only the tool's own arguments: no key, no config, nothing from your chats. The API takes up to 60 calls a minute from one IP address. The tool waits once, for a minute at most, when the API asks the tool to wait. The crate stores nothing.

## Links

- Datacircle: https://datacircle.dev
- The API: https://datacircle.dev/api
- The API's spec: https://api.datacircle.dev/v1/openapi.json
- What each key means: https://datacircle.dev/fields

MIT license.
