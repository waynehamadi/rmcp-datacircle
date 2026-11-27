//! The four tools as rmcp takes them: each one's definition (its input and output schemas, its annotations) and the
//! result of a call, as structured content and its JSON text, or an error result with what to send instead.

use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{Error, client::Client, logic};

pub(crate) const LIST_APIS: &str = "datacircle_list_apis";
pub(crate) const BEST_PLANS: &str = "datacircle_best_plans";
pub(crate) const SEARCH_PLANS: &str = "datacircle_search_plans";
pub(crate) const PROVIDER: &str = "datacircle_provider";

const LIST_APIS_ABOUT: &str = "Lists the GTM APIs on Datacircle's sheet (https://datacircle.dev), with each API's slug, how many plans and providers it has and its best plan. Call it for an API's slug you don't know: datacircle_best_plans takes a slug, like email-finder. A provider's plans are in datacircle_search_plans and datacircle_provider.";
const BEST_PLANS_ABOUT: &str = "Gives an API's plans best first by their score out of 100 on Datacircle's benchmark (https://datacircle.dev), each with its rank, provider, plan, $ per 1,000 calls and its page. It takes an API's slug, never a provider's: a provider's plans are in datacircle_search_plans. One provider may hold several of the best plans. perThousand is a plan's price of 1,000 items in dollars, pay the plan's whole price in its currency.";
const SEARCH_PLANS_ABOUT: &str = "Searches every plan on Datacircle's sheet (https://datacircle.dev), or one API's, by provider or plan name, sorted by score, provider or price. A provider's plans: its name as query, like bright data, and sort price for its cheapest. Gives how many were found and up to limit of them. perThousand is a plan's price of 1,000 items in dollars, pay the plan's whole price in its currency.";
const PROVIDER_ABOUT: &str = "Gives the APIs one provider sells and its rank on each on Datacircle's benchmark (https://datacircle.dev), with its best plan there, its best rank first. total counts the APIs it sells, ranked those where it has a rank (its place among the API's providers, 1 the best).";

fn text(about: &str) -> Value {
    json!({ "type": "string", "description": about })
}

fn count(low: i64, high: Option<i64>, default: Option<i64>, about: &str) -> Value {
    let mut field = json!({ "type": "integer", "minimum": low, "description": about });
    if let Some(high) = high {
        field["maximum"] = json!(high);
    }
    if let Some(default) = default {
        field["default"] = json!(default);
    }
    field
}

fn object(properties: Value, required: &[&str]) -> JsonObject {
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), properties);
    if !required.is_empty() {
        schema.insert("required".to_owned(), json!(required));
    }
    schema
}

fn input(name: &str) -> JsonObject {
    match name {
        LIST_APIS => object(
            json!({
                "query": text("Words of the API's name or slug, like email or linkedin profile: the APIs that hold the most of them come first. The sheet's order when left out"),
                "limit": count(1, Some(200), Some(10), "How many APIs to give, 1 to 200. 10 when left out"),
            }),
            &[],
        ),
        BEST_PLANS => object(
            json!({
                "slug": text("The API's slug, like email-finder or linkedin-profile, as datacircle_list_apis gives it"),
                "limit": count(1, Some(50), Some(10), "How many plans to give, 1 to 50. 10 when left out"),
            }),
            &["slug"],
        ),
        SEARCH_PLANS => object(
            json!({
                "api": text("Only this API's plans: its name or slug, like email-finder. A provider's name goes in query, never here. Every API when left out"),
                "query": text("Words that must all appear in the provider's or plan's name, like bright data"),
                "sort": {
                    "type": "string",
                    "enum": ["score", "provider", "price"],
                    "default": "score",
                    "description": "score (best first, the default), provider (A to Z) or price ($ per 1,000, lowest first)",
                },
                "limit": count(1, Some(50), Some(10), "How many plans to give, 1 to 50. 10 when left out"),
                "offset": count(0, None, Some(0), "How many found plans to skip first. 0 when left out"),
            }),
            &[],
        ),
        _ => object(
            json!({
                "slug": text("The provider's slug, like bright-data, or its name, like Bright Data: the name is made a slug"),
                "limit": count(1, Some(200), Some(10), "How many of its APIs to give, its best rank first, 1 to 200. 10 when left out"),
            }),
            &["slug"],
        ),
    }
}

/// One plan as the tools give it: the keys that choose a plan (the rest is on its page).
fn plan() -> Value {
    json!({
        "type": "object",
        "properties": {
            "api": text("The API, like Employee email"),
            "rank": { "type": "integer", "description": "Its place among the API's scored plans, 1 the best" },
            "provider": text("Who sells it"),
            "plan": text("Its name on the provider's pricing page"),
            "score": { "type": "number", "description": "Its score out of 100 on Datacircle's benchmark" },
            "perThousand": { "type": "number", "description": "The price of 1,000 items, in dollars" },
            "notFoundPerThousand": { "type": "number", "description": "What 1,000 items the provider has no answer for cost, in dollars" },
            "billing": text("How you pay: free, on-demand, one-time, monthly..."),
            "pay": { "type": "number", "description": "The plan's whole price, in its currency" },
            "currency": text("The currency of pay, like USD"),
            "page": text("The plan's page on datacircle.dev"),
        },
    })
}

fn output(name: &str) -> JsonObject {
    let note = text("What the tool did with arguments no API's or provider's name matched, where it did");
    match name {
        LIST_APIS => object(
            json!({
                "total": { "type": "integer", "description": "How many APIs matched" },
                "apis": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "api": text("The API's name"),
                            "slug": text("Its slug, which datacircle_best_plans takes"),
                            "plans": { "type": "integer", "description": "How many plans it has" },
                            "providers": { "type": "integer", "description": "How many providers sell it" },
                            "best": {
                                "type": "object",
                                "description": "The plan the sheet ranks first",
                                "properties": {
                                    "provider": text("Who sells it"),
                                    "plan": text("Its name"),
                                    "score": { "type": "number" },
                                    "perThousand": { "type": "number" },
                                },
                            },
                            "bestPage": text("The page that ranks its providers"),
                        },
                    },
                },
                "note": note,
            }),
            &["total", "apis"],
        ),
        BEST_PLANS => object(
            json!({
                "api": text("The API's name"),
                "slug": text("Its slug"),
                "sheetPage": text("Its sheet on datacircle.dev, every plan and its price"),
                "bestPage": text("The page that ranks its providers"),
                "total": { "type": "integer", "description": "How many plans it has" },
                "plans": { "type": "array", "items": plan() },
            }),
            &["plans"],
        ),
        SEARCH_PLANS => object(
            json!({
                "total": { "type": "integer", "description": "How many plans were found" },
                "offset": { "type": "integer", "description": "How many found plans were skipped" },
                "plans": { "type": "array", "items": plan() },
                "note": note,
            }),
            &["total", "plans"],
        ),
        _ => object(
            json!({
                "about": text("Its counts in words"),
                "total": { "type": "integer", "description": "How many APIs it sells" },
                "ranked": { "type": "integer", "description": "On how many of them it has a rank" },
                "provider": text("Its name"),
                "slug": text("Its slug"),
                "website": text("Its website"),
                "apis": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "api": text("The API's name"),
                            "slug": text("Its slug"),
                            "rank": { "type": "integer", "description": "Its place among the API's providers, 1 the best" },
                            "bestPage": text("The page that ranks the API's providers"),
                            "plans": { "type": "integer", "description": "How many plans it has there" },
                            "best": {
                                "type": "object",
                                "description": "Its best plan there",
                                "properties": {
                                    "plan": text("Its name"),
                                    "score": { "type": "number" },
                                    "perThousand": { "type": "number" },
                                    "page": text("Its page on datacircle.dev"),
                                },
                            },
                        },
                    },
                },
            }),
            &["about", "total", "ranked", "apis"],
        ),
    }
}

fn definition(name: &'static str, title: &str, about: &'static str) -> Tool {
    Tool::new(name, about, Arc::new(input(name)))
        .with_title(title)
        .with_raw_output_schema(Arc::new(output(name)))
        .with_annotations(
            ToolAnnotations::with_title(title)
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(true),
        )
}

pub(crate) fn definitions() -> Vec<Tool> {
    vec![
        definition(LIST_APIS, "List Datacircle's GTM APIs", LIST_APIS_ABOUT),
        definition(BEST_PLANS, "Best plans of a GTM API", BEST_PLANS_ABOUT),
        definition(SEARCH_PLANS, "Search GTM API plans", SEARCH_PLANS_ABOUT),
        definition(PROVIDER, "A GTM API provider's ranks", PROVIDER_ABOUT),
    ]
}

/// The arguments a client sent, decoded: none at all when it sent none.
fn arguments<A: DeserializeOwned + Default>(name: &str, arguments: Option<JsonObject>) -> Result<A, Error> {
    match arguments {
        None => Ok(A::default()),
        Some(object) => serde_json::from_value(Value::Object(object)).map_err(|error| {
            Error::Advice(format!(
                "The arguments are no JSON object of {name}'s parameters: {error}"
            ))
        }),
    }
}

/// One call of the tool named `name`: the API's answer as a JSON object, its keys of no value left out.
pub(crate) async fn answer(client: &Client, name: &str, args: Option<JsonObject>) -> Result<Value, Error> {
    let mut answer = match name {
        LIST_APIS => logic::list_apis(client, arguments(name, args)?).await,
        BEST_PLANS => logic::best_plans(client, arguments(name, args)?).await,
        SEARCH_PLANS => logic::search_plans(client, arguments(name, args)?).await,
        PROVIDER => logic::provider(client, arguments(name, args)?).await,
        _ => {
            return Err(Error::Advice(format!(
                "No Datacircle tool is named {name:?}: its tools are {}",
                crate::NAMES.join(", ")
            )));
        }
    }?;
    if let Value::Object(keys) = &mut answer {
        keys.retain(|_, value| !value.is_null());
    }
    Ok(answer)
}

/// The result an MCP client reads: the answer as structured content and its JSON text, or an error result
/// (`isError`) whose text says what to send instead.
pub(crate) async fn call(client: &Client, name: &str, args: Option<JsonObject>) -> CallToolResult {
    match answer(client, name, args).await {
        Ok(answer) => CallToolResult::structured(answer),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
    }
}
