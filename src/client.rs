use std::time::Duration;

use reqwest::header::{ACCEPT, RETRY_AFTER, USER_AGENT};
use serde_json::Value;

use crate::Error;

/// The API's address.
pub const BASE_URL: &str = "https://api.datacircle.dev/v1";

const AGENT: &str = concat!("rmcp-datacircle/", env!("CARGO_PKG_VERSION"));

/// The longest a 429 is waited for, and the wait when its Retry-After says none.
const LONGEST_WAIT: u64 = 60;

#[derive(Debug)]
pub(crate) struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub(crate) fn new(base: String, http: reqwest::Client) -> Self {
        Self { base, http }
    }

    /// One GET of the API, its JSON decoded. A 429 is waited for once, as its Retry-After says.
    pub(crate) async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, Error> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base))
            .map_err(|error| Error::Network(format!("{error}: {}", self.base)))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut waited = false;
        loop {
            let response = self
                .http
                .get(url.clone())
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, AGENT)
                .send()
                .await
                .map_err(Error::from_reqwest)?;
            let status = response.status().as_u16();
            if status == 429 && !waited {
                waited = true;
                tokio::time::sleep(retry_after(response.headers().get(RETRY_AFTER))).await;
                continue;
            }
            let body = response.bytes().await.map_err(Error::from_reqwest)?;
            if (400..500).contains(&status) {
                let words = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|answer| answer.get("error")?.as_str().map(str::to_owned))
                    .filter(|words| !words.is_empty());
                return Err(Error::Api {
                    status,
                    message: words.unwrap_or_else(|| format!("Datacircle's API answered {status}")),
                });
            }
            if status != 200 {
                return Err(Error::Status(status));
            }
            return serde_json::from_slice(&body).map_err(|error| Error::Decode(error.to_string()));
        }
    }
}

fn retry_after(header: Option<&reqwest::header::HeaderValue>) -> Duration {
    let seconds = header
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| (1..=LONGEST_WAIT).contains(seconds))
        .unwrap_or(LONGEST_WAIT);
    Duration::from_secs(seconds)
}
