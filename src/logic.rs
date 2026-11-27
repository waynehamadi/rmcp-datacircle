//! What each tool does with the API's answers: the keys a model needs of a plan, and words that tell it what to send
//! when it sent a provider where an API goes, or a slug where a name does.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::{Error, client::Client};

/// `datacircle_list_apis`' arguments.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ListApisArgs {
    /// Words of the API's name or slug, like `email` or `linkedin profile`: the APIs that hold the most of them come
    /// first. The sheet's order when left out.
    #[serde(deserialize_with = "text")]
    pub query: Option<String>,
    /// How many APIs to give, 1 to 200. 10 when left out.
    #[serde(deserialize_with = "count")]
    pub limit: Option<i64>,
}

/// `datacircle_best_plans`' arguments.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct BestPlansArgs {
    /// The API's slug, like `email-finder`, as `datacircle_list_apis` gives it.
    #[serde(deserialize_with = "text")]
    pub slug: Option<String>,
    /// How many plans to give, 1 to 50. 10 when left out.
    #[serde(deserialize_with = "count")]
    pub limit: Option<i64>,
}

/// `datacircle_search_plans`' arguments.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SearchPlansArgs {
    /// Only this API's plans: its name or slug, like `email-finder`. Every API when left out.
    #[serde(deserialize_with = "text")]
    pub api: Option<String>,
    /// Words that must all appear in the provider's or plan's name, like `bright data`.
    #[serde(deserialize_with = "text")]
    pub query: Option<String>,
    /// `score` (best first, the default), `provider` (A to Z) or `price` ($ per 1,000, lowest first).
    #[serde(deserialize_with = "text")]
    pub sort: Option<String>,
    /// How many plans to give, 1 to 50. 10 when left out.
    #[serde(deserialize_with = "count")]
    pub limit: Option<i64>,
    /// How many found plans to skip first. 0 when left out.
    #[serde(deserialize_with = "count")]
    pub offset: Option<i64>,
}

/// `datacircle_provider`'s arguments.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProviderArgs {
    /// The provider's slug, like `bright-data`, or its name, like `Bright Data`: the name is made a slug.
    #[serde(deserialize_with = "text")]
    pub slug: Option<String>,
    /// How many of its APIs to give, its best rank first, 1 to 200. 10 when left out.
    #[serde(deserialize_with = "count")]
    pub limit: Option<i64>,
}

/// A whole number a model may send as a number, as text (`"3"`) or as null: left out.
fn count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Number(number) => number.as_i64().or_else(|| number.as_f64().map(|n| n as i64)),
        Value::String(text) => text.trim().parse::<f64>().ok().map(|n| n as i64),
        Value::Null => None,
        _ => return Err(serde::de::Error::custom("a whole number")),
    })
}

/// Text a model may send as a number too, or as null: left out.
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => Some(text),
        Value::Number(number) => Some(number.to_string()),
        Value::Null => None,
        _ => return Err(serde::de::Error::custom("text")),
    })
}

/// A plan holds about 90 keys (its terms of use, its aggregators...); these choose a plan, the rest is on its page:
/// 10 plans of these keys take about 2,500 bytes of a model's context.
const PLAN_KEYS: [&str; 11] = [
    "api",
    "rank",
    "provider",
    "plan",
    "score",
    "perThousand",
    "notFoundPerThousand",
    "billing",
    "pay",
    "currency",
    "page",
];

/// Where a provider's plans are, for a model that sent a provider's name to an API's tool.
const FOR_PROVIDER: &str =
    "A provider's plans: datacircle_search_plans with its name as query, like bright data, or datacircle_provider.";

pub(crate) async fn list_apis(client: &Client, args: ListApisArgs) -> Result<Value, Error> {
    let answer = client.get("/apis", &[]).await?;
    let query = args.query.unwrap_or_default().trim().to_owned();
    let lowered = query.to_lowercase().replace('-', " ");
    let words: Vec<&str> = lowered.split_whitespace().collect();
    let mut found: Vec<(usize, Value)> = Vec::new();
    for api in items(&answer) {
        let name = format!("{} {}", str_of(api, "api"), str_of(api, "slug").replace('-', " ")).to_lowercase();
        let matched = words.iter().filter(|word| name.contains(*word)).count();
        if matched > 0 || words.is_empty() {
            found.push((
                matched,
                brief(api, &["api", "slug", "plans", "providers", "best", "bestPage"]),
            ));
        }
    }
    // The most words matched first, the sheet's order within each.
    found.sort_by_key(|(matched, _)| std::cmp::Reverse(*matched));
    let limit = clamp(args.limit, 10, 1, 200);
    let mut out = json!({
        "total": found.len(),
        "apis": found.iter().take(limit).map(|(_, api)| api.clone()).collect::<Vec<_>>(),
    });
    if !words.is_empty() && found.first().is_none_or(|(matched, _)| *matched < words.len()) {
        // A provider's name sent as an API's, like Bright Data, whose data matches many APIs: where its plans are.
        out["note"] = json!(format!(
            "No API's name holds every word of {query:?}. A provider's plans: datacircle_search_plans with its name as query, or datacircle_provider."
        ));
    }
    Ok(out)
}

pub(crate) async fn best_plans(client: &Client, args: BestPlansArgs) -> Result<Value, Error> {
    let slug = slugify(&args.slug.unwrap_or_default());
    if slug.is_empty() {
        return Err(Error::Advice(format!(
            "Give an API's slug, like email-finder: datacircle_list_apis gives every slug. {FOR_PROVIDER}"
        )));
    }
    let mut query = Vec::new();
    if let Some(limit) = args.limit.filter(|limit| *limit > 0) {
        query.push(("limit", limit.clamp(1, 50).to_string()));
    }
    let mut answer = match client.get(&format!("/apis/{slug}/best"), &query).await {
        Err(Error::Api { status: 404, message }) => {
            // A provider's name sent as the API's slug: the API's words, and where a provider's plans are.
            return Err(Error::Api {
                status: 404,
                message: format!("{message}. datacircle_list_apis gives every API's slug. {FOR_PROVIDER}"),
            });
        }
        other => object(other?)?,
    };
    answer["plans"] = brief_plans(&answer["plans"]);
    Ok(answer)
}

pub(crate) async fn search_plans(client: &Client, args: SearchPlansArgs) -> Result<Value, Error> {
    let mut api = args.api.unwrap_or_default().trim().to_owned();
    let mut query = args.query.unwrap_or_default().trim().to_owned();
    let sort = args.sort.unwrap_or_default().trim().to_owned();
    let mut note = String::new();
    let mut answer = search(client, &api, &query, &sort, args.limit, args.offset).await;
    if let Err(Error::Api { status: 400, message }) = &answer
        && message.contains("api must be")
        && let Some(slug) = api_slug(client, &api).await
    {
        // An API's name or slug in another case, like Email-finder or employee email, which the API takes only as
        // /apis writes it: its slug.
        api = slug;
        answer = search(client, &api, &query, &sort, args.limit, args.offset).await;
    }
    if let Err(Error::Api { status: 400, message }) = &answer
        && message.contains("api must be")
    {
        if !query.is_empty() {
            // The API's answer names all 150 APIs: where they are instead.
            return Err(Error::Api {
                status: 400,
                message: format!("No API is named {api:?}. datacircle_list_apis gives every API's name and slug."),
            });
        }
        // A provider's name sent as the API: the plans whose provider or plan name holds it.
        query = std::mem::take(&mut api);
        answer = search(client, &api, &query, &sort, args.limit, args.offset).await;
        note = format!("No API is named {query:?}: these are the plans whose provider or plan name holds it.");
    }
    let mut answer = object(answer?)?;
    if answer["total"].as_f64() == Some(0.0) && query.contains('-') {
        // A slug sent as a name, like bright-data, which no name holds: its words, like bright data. A name with a
        // hyphen, like A-Leads, is found as it is first.
        let words = query.replace('-', " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if let Ok(again) = search(client, &api, &words, &sort, args.limit, args.offset)
            .await
            .and_then(object)
        {
            answer = again;
            note = format!(
                "{note} No provider's or plan's name holds {query:?}: these are the plans whose name holds {words:?}."
            )
            .trim()
            .to_owned();
        }
    }
    if !note.is_empty() {
        answer["note"] = json!(note);
    }
    answer["plans"] = brief_plans(&answer["plans"]);
    Ok(answer)
}

/// The slug of the API whose name or slug is `api` in any case, as `/apis` gives it, if it is another way of writing it.
async fn api_slug(client: &Client, api: &str) -> Option<String> {
    let wanted = slugify(api);
    let answer = client.get("/apis", &[]).await.ok()?;
    items(&answer)
        .find(|item| slugify(str_of(item, "slug")) == wanted || slugify(str_of(item, "api")) == wanted)
        .map(|item| str_of(item, "slug").to_owned())
        .filter(|slug| !slug.is_empty() && slug != api)
}

async fn search(
    client: &Client,
    api: &str,
    query: &str,
    sort: &str,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Value, Error> {
    let mut params = Vec::new();
    if !api.is_empty() {
        params.push(("api", api.to_owned()));
    }
    if !query.is_empty() {
        params.push(("query", query.to_owned()));
    }
    if matches!(sort, "score" | "provider" | "price") {
        params.push(("sort", sort.to_owned()));
    }
    if let Some(limit) = limit.filter(|limit| *limit > 0) {
        params.push(("limit", limit.clamp(1, 50).to_string()));
    }
    if let Some(offset) = offset.filter(|offset| *offset > 0) {
        params.push(("offset", offset.to_string()));
    }
    client.get("/plans", &params).await
}

pub(crate) async fn provider(client: &Client, args: ProviderArgs) -> Result<Value, Error> {
    let slug = slugify(&args.slug.unwrap_or_default());
    if slug.is_empty() {
        return Err(Error::Advice(
            "Give a provider's slug or name, like bright-data or Bright Data".to_owned(),
        ));
    }
    let answer = match client.get(&format!("/providers/{slug}"), &[]).await {
        Err(Error::Api { status: 404, message }) => {
            // The API's words name its own address: the tool that finds a provider's name instead.
            return Err(Error::Api {
                status: 404,
                message: format!(
                    "{message}. datacircle_search_plans with words of its name as query gives each plan's provider."
                ),
            });
        }
        other => object(other?)?,
    };
    let mut apis: Vec<Value> = items(&answer["apis"])
        .map(|entry| {
            let plans: Vec<&Map<String, Value>> = items(&entry["plans"]).collect();
            let mut kept = brief(entry, &["api", "slug", "rank", "bestPage"]);
            kept["plans"] = json!(plans.len());
            if let Some(best) = best_plan(&plans) {
                kept["best"] = brief(best, &["plan", "score", "perThousand", "page"]);
            }
            kept
        })
        .collect();
    // Its best rank first, the APIs where it has none last, in the API's order.
    apis.sort_by(|a, b| match (a["rank"].as_f64(), b["rank"].as_f64()) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let best_rank = apis.first().and_then(|api| api["rank"].as_f64());
    let ranked = apis.iter().filter(|api| api["rank"].is_number()).count();
    let best_at: Vec<String> = apis
        .iter()
        .filter(|api| best_rank.is_some() && api["rank"].as_f64() == best_rank)
        .map(|api| api["api"].as_str().unwrap_or_default().to_owned())
        .collect();
    let name = match answer["provider"].as_str() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => slug.clone(),
    };
    // The counts in words, first of the answer's keys: gpt-oss:120b read total, after 63 APIs, as where it is ranked.
    let mut about = format!(
        "{name} has plans on {} APIs on Datacircle and a rank on {ranked} of them (1 is the best).",
        apis.len()
    );
    if let (Some(rank), true) = (apis.first().map(|api| api["rank"].clone()), ranked > 0) {
        about.push_str(&format!(" Its best rank: {rank}, on {}.", best_at.join(", ")));
    }
    // Built in this order, as an app whose serde_json keeps keys in order (its preserve_order) writes them.
    let mut out = Map::new();
    out.insert("about".to_owned(), json!(about));
    out.insert("total".to_owned(), json!(apis.len()));
    out.insert("ranked".to_owned(), json!(ranked));
    apis.truncate(clamp(args.limit, 10, 1, 200));
    if let Value::Object(rest) = answer {
        for (key, value) in rest {
            out.entry(key).or_insert(value);
        }
    }
    out.insert("apis".to_owned(), Value::Array(apis));
    Ok(Value::Object(out))
}

/// A provider's best plan on an API: its lowest rank, else its first.
fn best_plan<'a>(plans: &[&'a Map<String, Value>]) -> Option<&'a Map<String, Value>> {
    plans
        .iter()
        .filter_map(|plan| plan.get("rank").and_then(Value::as_f64).map(|rank| (rank, *plan)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, plan)| plan)
        .or_else(|| plans.first().copied())
}

/// The API's rule for a slug: the name in lowercase letters and digits, the rest a hyphen.
pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in name.trim().to_lowercase().nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        if c.is_ascii_alphanumeric() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c);
        } else {
            dash = true;
        }
    }
    slug
}

/// An answer the tool adds keys to: a JSON object, as the spec gives every answer but `/apis`'.
fn object(answer: Value) -> Result<Value, Error> {
    if answer.is_object() {
        Ok(answer)
    } else {
        Err(Error::Decode("an answer that is no JSON object".to_owned()))
    }
}

fn clamp(value: Option<i64>, default: usize, low: i64, high: i64) -> usize {
    match value.filter(|value| *value > 0) {
        Some(value) => value.clamp(low, high) as usize,
        None => default,
    }
}

fn items(value: &Value) -> impl Iterator<Item = &Map<String, Value>> {
    value.as_array().into_iter().flatten().filter_map(Value::as_object)
}

fn str_of<'a>(item: &'a Map<String, Value>, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn brief(item: &Map<String, Value>, keys: &[&str]) -> Value {
    let kept: Map<String, Value> = keys
        .iter()
        .filter_map(|key| {
            item.get(*key)
                .filter(|value| !value.is_null())
                .map(|value| ((*key).to_owned(), value.clone()))
        })
        .collect();
    Value::Object(kept)
}

fn brief_plans(plans: &Value) -> Value {
    Value::Array(items(plans).map(|plan| brief(plan, &PLAN_KEYS)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_as_the_api_makes_them() {
        assert_eq!(slugify("Bright Data"), "bright-data");
        assert_eq!(slugify("  A-Leads "), "a-leads");
        assert_eq!(slugify("Société Générale"), "societe-generale");
        assert_eq!(slugify("People Data Labs (PDL)"), "people-data-labs-pdl");
        assert_eq!(slugify("--"), "");
    }

    #[test]
    fn counts_as_a_model_sends_them() {
        let args: SearchPlansArgs =
            serde_json::from_value(json!({"limit": "3", "offset": 2.0, "api": null, "query": 7})).unwrap_or_default();
        assert_eq!(args.limit, Some(3));
        assert_eq!(args.offset, Some(2));
        assert_eq!(args.api, None);
        assert_eq!(args.query.as_deref(), Some("7"));
        assert!(serde_json::from_value::<SearchPlansArgs>(json!({"limit": [1]})).is_err());
    }
}
