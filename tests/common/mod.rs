//! A small HTTP server on 127.0.0.1 for the tests: each request handed to a function, its answer written back, the
//! connection closed.

#![allow(dead_code)]

use std::{future::Future, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A request as the server read it.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// Its path and query, as sent.
    pub target: String,
    pub body: Vec<u8>,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    /// A query parameter, decoded.
    pub fn query(&self, key: &str) -> Option<String> {
        let url = reqwest::Url::parse(&format!("http://x{}", self.target)).ok()?;
        url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }
}

/// An answer: its status, its headers, its body, and how long to wait before it.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Option<Duration>,
}

impl Answer {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into().into_bytes(),
            delay: None,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

/// Serves `handle` on 127.0.0.1 until the test ends: its address, like `http://127.0.0.1:41234`.
pub async fn serve<F, Fut>(handle: F) -> String
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Answer> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port on 127.0.0.1");
    let address = format!("http://{}", listener.local_addr().expect("its address"));
    let handle = Arc::new(handle);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move {
                let Some(request) = read(&mut socket).await else { return };
                let answer = handle(request).await;
                if let Some(delay) = answer.delay {
                    tokio::time::sleep(delay).await;
                }
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    answer.status,
                    answer.body.len()
                );
                for (name, value) in &answer.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&answer.body).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    address
}

async fn read(socket: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..n]);
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data[end..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request { method, target, body })
}

/// An address nothing answers on: a port bound, then let go.
pub async fn closed_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port on 127.0.0.1");
    let address = format!("http://{}", listener.local_addr().expect("its address"));
    drop(listener);
    address
}
