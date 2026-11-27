//! Each tool called the ways an rmcp server serves it, through rmcp's own client: on an app's server beside its own
//! tool (`#[tool_router]` and `#[tool_handler(router = self.tool_router)]`, in process), on `Datacircle` as a server
//! of its own (in process), and on the `datacircle-mcp` binary over stdio. On a stub of the API on 127.0.0.1: its
//! answers, its failures (404, 400, 429, 502, HTML, down, a timeout) and what the model reads of each, each answer
//! checked against its tool's output schema. `DATACIRCLE_LIVE=1` calls the API itself too.

mod common;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use common::{Answer, Request, closed_address, serve};
use rmcp::{
    RoleClient, ServerHandler, ServiceExt,
    handler::server::tool::ToolRouter,
    model::{CallToolRequestParams, CallToolResult, JsonObject, Tool},
    service::RunningService,
    tool, tool_handler, tool_router,
    transport::TokioChildProcess,
};
use rmcp_datacircle::{Datacircle, NAMES};
use serde_json::{Value, json};

/// An app's server: its own tool, and Datacircle's four beside it.
#[derive(Clone)]
struct App {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl App {
    fn new(datacircle: &Datacircle) -> Self {
        Self {
            tool_router: Self::tool_router() + datacircle.tool_router(),
        }
    }

    #[tool(description = "The app's own tool: says hello")]
    fn hello(&self) -> String {
        "Hello from the app".to_owned()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for App {}

/// The three ways a client reaches the tools.
#[derive(Clone, Copy, Debug)]
enum How {
    /// An app's server, the four beside its own tool, in process.
    App,
    /// `Datacircle` as a server of its own, in process.
    Server,
    /// The `datacircle-mcp` binary, over stdio.
    Stdio,
}

const HOWS: [How; 3] = [How::App, How::Server, How::Stdio];

type Client = RunningService<RoleClient, ()>;

async fn connect(how: How, base: &str) -> Client {
    let datacircle = Datacircle::builder()
        .base_url(base)
        .timeout(Duration::from_secs(2))
        .build();
    match how {
        How::App | How::Server => {
            let (server_side, client_side) = tokio::io::duplex(1 << 16);
            tokio::spawn(async move {
                if matches!(how, How::App) {
                    if let Ok(running) = App::new(&datacircle).serve(server_side).await {
                        let _ = running.waiting().await;
                    }
                } else if let Ok(running) = datacircle.serve(server_side).await {
                    let _ = running.waiting().await;
                }
            });
            ().serve(client_side).await.expect("an MCP session")
        }
        How::Stdio => {
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_datacircle-mcp"));
            command.env("DATACIRCLE_BASE_URL", base);
            ().serve(TokioChildProcess::new(command).expect("datacircle-mcp started"))
                .await
                .expect("an MCP session")
        }
    }
}

async fn call(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name.to_owned());
    if let Value::Object(arguments) = arguments {
        params = params.with_arguments(arguments);
    }
    client.call_tool(params).await.expect("a tool result")
}

/// The text a client reads of a result.
fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `value` fits `schema`, as far as the tools' schemas go: type, properties, required, items, enum, bounds.
fn fits(schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    if let Some(kind) = schema["type"].as_str() {
        let ok = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !ok {
            return Err(format!("{at}: {value} is no {kind}"));
        }
    }
    if let Some(required) = schema["required"].as_array() {
        for key in required.iter().filter_map(Value::as_str) {
            if value.get(key).is_none() {
                return Err(format!("{at}: no {key}"));
            }
        }
    }
    if let (Some(properties), Some(object)) = (schema["properties"].as_object(), value.as_object()) {
        for (key, field) in object {
            if let Some(property) = properties.get(key) {
                fits(property, field, &format!("{at}.{key}"))?;
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, item) in array.iter().enumerate() {
            fits(items, item, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

/// A result a client reads as data: structured content, its text the same JSON, fitting the tool's output schema.
fn data(tools: &[Tool], name: &str, result: &CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{name}: {}", text(result));
    let value = result.structured_content.clone().expect("structured content");
    let parsed: Value = serde_json::from_str(&text(result)).expect("its text is JSON");
    assert_eq!(parsed, value, "{name}: its text is its structured content");
    let tool = tools.iter().find(|tool| tool.name == name).expect("listed");
    let schema = Value::Object(tool.output_schema.as_deref().cloned().expect("an output schema"));
    fits(&schema, &value, name).unwrap_or_else(|error| panic!("{error}"));
    value
}

/// An error result whose text says what to send instead.
fn failure(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "an error result: {result:?}");
    assert!(result.structured_content.is_none());
    text(result)
}

fn object(value: Value) -> JsonObject {
    value.as_object().cloned().unwrap_or_default()
}

async fn stub(paths: Arc<Mutex<Vec<String>>>) -> String {
    let tries = Arc::new(AtomicUsize::new(0));
    serve(move |request: Request| {
        let (paths, tries) = (Arc::clone(&paths), Arc::clone(&tries));
        async move {
            paths.lock().unwrap().push(request.target.clone());
            let api = request.query("api").unwrap_or_default();
            let query = request.query("query").unwrap_or_default();
            match request.path() {
                "/apis" => Answer::json(
                    200,
                    r#"[{"api":"Employee profile","slug":"linkedin-profile","plans":4,"sheetPage":"s","best":null},{"api":"Employee email","slug":"email-finder","plans":3,"providers":2,"best":{"provider":"A","plan":"P","score":70.5,"perThousand":1.5}}]"#,
                ),
                "/apis/email-finder/best" => Answer::json(
                    200,
                    r#"{"api":"Employee email","slug":"email-finder","sheetPage":"s","bestPage":null,"total":3,"plans":[{"provider":"A","rank":1,"score":70.5,"perThousand":1.5,"terms":"long","website":null}]}"#,
                ),
                "/plans" if ["Bright Data", "bright-data", "Email-finder", "employee email"].contains(&api.as_str()) => {
                    Answer::json(400, r#"{"error":"api must be one of: Employee email"}"#)
                }
                "/plans" if query.contains('-') => Answer::json(200, r#"{"total":0,"offset":0,"plans":[]}"#),
                "/plans" => Answer::json(
                    200,
                    r#"{"total":1,"offset":0,"plans":[{"provider":"Bright Data","rank":2,"pay":49.99,"page":"p"}]}"#,
                ),
                "/providers/bright-data" => Answer::json(
                    200,
                    r#"{"provider":"Bright Data","slug":"bright-data","website":null,"apis":[{"api":"Employee profile","slug":"linkedin-profile","rank":null,"plans":[{"plan":"C"}]},{"api":"Employee email","slug":"email-finder","rank":4,"plans":[{"plan":"B","rank":9},{"plan":"A","rank":4,"score":60}]}]}"#,
                ),
                "/providers/down" => Answer::status(502),
                "/providers/html" => Answer::status(200).header("Content-Type", "text/html"),
                "/providers/slow" => Answer::json(200, "{}").after(Duration::from_secs(5)),
                "/providers/busy" => {
                    if tries.fetch_add(1, Ordering::SeqCst) % 2 == 0 {
                        Answer::json(429, r#"{"error":"too many calls"}"#).header("Retry-After", "1")
                    } else {
                        Answer::json(200, r#"{"provider":"Busy","apis":[]}"#)
                    }
                }
                "/providers/full" => Answer::json(429, r#"{"error":"60 calls a minute at most"}"#).header("Retry-After", "1"),
                _ => Answer::json(404, r#"{"error":"no API has the slug nope"}"#),
            }
        }
    })
    .await
}

#[test]
fn definitions() {
    let tools = Datacircle::new().tools();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, NAMES);
    for tool in &tools {
        assert!(
            tool.description
                .as_deref()
                .unwrap_or_default()
                .contains("https://datacircle.dev")
        );
        assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
        assert!(tool.input_schema["properties"].is_object(), "{}", tool.name);
        let output = tool.output_schema.as_deref().expect("an output schema");
        assert_eq!(output["type"], "object", "{}", tool.name);
        // No type of two kinds (like ["string", "null"]): some clients' schema dialects take none.
        let sent = serde_json::to_string(tool).unwrap();
        assert!(!sent.contains(r#""type":["#), "{}", tool.name);
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.open_world_hint, Some(true));
    }
    let required = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap()
            .input_schema
            .get("required")
            .cloned()
    };
    assert_eq!(required("datacircle_best_plans"), Some(json!(["slug"])));
    assert_eq!(required("datacircle_provider"), Some(json!(["slug"])));
    assert_eq!(required("datacircle_list_apis"), None);
    assert_eq!(required("datacircle_search_plans"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls() {
    let paths = Arc::new(Mutex::new(Vec::new()));
    let base = stub(Arc::clone(&paths)).await;
    for how in HOWS {
        let client = connect(how, &base).await;
        let tools = client.list_all_tools().await.expect("tools/list");
        let mut names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
        names.sort();
        let mut expected: Vec<String> = NAMES.iter().map(|name| name.to_string()).collect();
        if matches!(how, How::App) {
            expected.push("hello".to_owned());
        }
        expected.sort();
        assert_eq!(names, expected, "{how:?}");
        if matches!(how, How::App) {
            assert_eq!(text(&call(&client, "hello", json!({})).await), "Hello from the app");
        }

        // Answers, with what a client reads of each.
        let apis = data(
            &tools,
            "datacircle_list_apis",
            &call(&client, "datacircle_list_apis", json!({"query": "email"})).await,
        );
        assert_eq!(apis["total"], 1, "{how:?}");
        assert_eq!(apis["apis"][0]["slug"], "email-finder");
        let all = data(
            &tools,
            "datacircle_list_apis",
            &call(&client, "datacircle_list_apis", json!({})).await,
        );
        assert_eq!(all["total"], 2);
        assert!(all["apis"][0].get("best").is_none(), "a best of null left out");
        let none = data(
            &tools,
            "datacircle_list_apis",
            &call(&client, "datacircle_list_apis", json!({"query": "Bright Data"})).await,
        );
        assert!(none["note"].as_str().unwrap().contains("datacircle_search_plans"));

        let best = data(
            &tools,
            "datacircle_best_plans",
            &call(
                &client,
                "datacircle_best_plans",
                json!({"slug": "Email Finder", "limit": "2"}),
            )
            .await,
        );
        assert_eq!(best["plans"][0]["provider"], "A");
        assert!(
            best["plans"][0].get("terms").is_none(),
            "only the keys that choose a plan"
        );
        assert!(best.get("bestPage").is_none(), "a key of null left out");
        assert!(
            paths
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/apis/email-finder/best?limit=2")
        );

        let named = data(
            &tools,
            "datacircle_search_plans",
            &call(&client, "datacircle_search_plans", json!({"api": "Bright Data"})).await,
        );
        assert!(named["note"].as_str().unwrap().contains("No API is named"), "{how:?}");
        let cased = data(
            &tools,
            "datacircle_search_plans",
            &call(
                &client,
                "datacircle_search_plans",
                json!({"api": "Email-finder", "sort": "price"}),
            )
            .await,
        );
        assert_eq!(cased["total"], 1);
        assert!(
            paths
                .lock()
                .unwrap()
                .iter()
                .any(|path| path == "/plans?api=email-finder&sort=price")
        );
        let slug = data(
            &tools,
            "datacircle_search_plans",
            &call(
                &client,
                "datacircle_search_plans",
                json!({"query": "bright-data", "limit": null}),
            )
            .await,
        );
        assert_eq!(slug["plans"][0]["provider"], "Bright Data");
        assert!(
            slug["note"]
                .as_str()
                .unwrap()
                .contains(r#"whose name holds "bright data""#)
        );
        let wrong = failure(
            &call(
                &client,
                "datacircle_search_plans",
                json!({"api": "Bright Data", "query": "growth"}),
            )
            .await,
        );
        assert_eq!(
            wrong,
            r#"No API is named "Bright Data". datacircle_list_apis gives every API's name and slug."#
        );

        let provider = data(
            &tools,
            "datacircle_provider",
            &call(&client, "datacircle_provider", json!({"slug": "Bright Data"})).await,
        );
        assert_eq!(provider["apis"][0]["api"], "Employee email", "its best rank first");
        assert_eq!(provider["apis"][0]["best"]["plan"], "A");
        assert_eq!(provider["ranked"], 1);
        assert!(
            provider["about"]
                .as_str()
                .unwrap()
                .starts_with("Bright Data has plans on 2 APIs")
        );

        // Failures, as error results the model reads.
        let unknown = failure(&call(&client, "datacircle_best_plans", json!({"slug": "nope"})).await);
        assert!(
            unknown.starts_with("no API has the slug nope. datacircle_list_apis gives every API's slug."),
            "{unknown}"
        );
        assert!(failure(&call(&client, "datacircle_best_plans", json!({})).await).starts_with("Give an API's slug"));
        assert!(
            failure(&call(&client, "datacircle_provider", json!({"slug": "  "})).await)
                .starts_with("Give a provider's slug")
        );
        let bad = failure(&call(&client, "datacircle_search_plans", json!({"limit": [1]})).await);
        assert!(
            bad.starts_with("The arguments are no JSON object of datacircle_search_plans's parameters"),
            "{bad}"
        );
        assert_eq!(
            failure(&call(&client, "datacircle_provider", json!({"slug": "down"})).await),
            "Datacircle's API answered 502: try again later"
        );
        assert!(
            failure(&call(&client, "datacircle_provider", json!({"slug": "html"})).await)
                .starts_with("Datacircle's API answered no JSON")
        );
        if !matches!(how, How::Stdio) {
            // The binary keeps its own 30 s: the 2 s of the in-process servers' Datacircle.
            assert_eq!(
                failure(&call(&client, "datacircle_provider", json!({"slug": "slow"})).await),
                "Datacircle's API did not answer in time: try again later"
            );
        }
        let started = Instant::now();
        let busy = data(
            &tools,
            "datacircle_provider",
            &call(&client, "datacircle_provider", json!({"slug": "busy"})).await,
        );
        assert_eq!(busy["provider"], "Busy");
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "a 429 waited for as its Retry-After says"
        );
        assert_eq!(
            failure(&call(&client, "datacircle_provider", json!({"slug": "full"})).await),
            "60 calls a minute at most"
        );

        // A tool that is none of the server's: a protocol error, as rmcp gives it.
        let missing = client.call_tool(CallToolRequestParams::new("datacircle_nothing")).await;
        assert!(missing.is_err(), "{how:?}: {missing:?}");
        client.cancel().await.expect("closed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn down() {
    let base = closed_address().await;
    for how in HOWS {
        let client = connect(how, &base).await;
        let words = failure(&call(&client, "datacircle_list_apis", json!({})).await);
        assert!(
            words.starts_with("Datacircle's API could not be reached"),
            "{how:?}: {words}"
        );
        client.cancel().await.expect("closed");
    }
}

#[tokio::test]
async fn direct() {
    let paths = Arc::new(Mutex::new(Vec::new()));
    let base = stub(paths).await;
    let datacircle = Datacircle::builder().base_url(&base).build();
    let result = datacircle
        .call("datacircle_best_plans", Some(object(json!({"slug": "email-finder"}))))
        .await;
    assert_eq!(result.structured_content.unwrap()["plans"][0]["rank"], 1);
    let none = datacircle.call("datacircle_nothing", None).await;
    assert!(failure(&none).contains("its tools are datacircle_list_apis"));
    let args = rmcp_datacircle::ProviderArgs {
        slug: Some("Bright Data".to_owned()),
        limit: Some(1),
    };
    assert_eq!(
        datacircle.provider(args).await.unwrap()["apis"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// The API itself, through each way: `DATACIRCLE_LIVE=1 cargo test --test tools live`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live() {
    if std::env::var("DATACIRCLE_LIVE").as_deref() != Ok("1") {
        return;
    }
    for how in HOWS {
        let client = connect(how, rmcp_datacircle::BASE_URL).await;
        let tools = client.list_all_tools().await.expect("tools/list");
        let apis = data(
            &tools,
            "datacircle_list_apis",
            &call(
                &client,
                "datacircle_list_apis",
                json!({"query": "email finder", "limit": 3}),
            )
            .await,
        );
        let slug = apis["apis"][0]["slug"].as_str().expect("a slug").to_owned();
        let best = data(
            &tools,
            "datacircle_best_plans",
            &call(&client, "datacircle_best_plans", json!({"slug": slug, "limit": 3})).await,
        );
        let provider = best["plans"][0]["provider"].as_str().expect("a provider").to_owned();
        let plans = data(
            &tools,
            "datacircle_search_plans",
            &call(
                &client,
                "datacircle_search_plans",
                json!({"api": "Email-finder", "sort": "price", "limit": 3}),
            )
            .await,
        );
        let ranks = data(
            &tools,
            "datacircle_provider",
            &call(&client, "datacircle_provider", json!({"slug": provider, "limit": 3})).await,
        );
        let none = failure(&call(&client, "datacircle_best_plans", json!({"slug": "bright-data"})).await);
        println!(
            "{how:?}: {} APIs for email finder, first {slug}; best {} at ${}/1,000; {} plans by price, first {} ${}; {}; bright-data as an API: {none}",
            apis["total"],
            provider,
            best["plans"][0]["perThousand"],
            plans["total"],
            plans["plans"][0]["provider"],
            plans["plans"][0]["perThousand"],
            ranks["about"]
        );
        client.cancel().await.expect("closed");
    }
}
