#![allow(dead_code)] // Each integration binary uses a different fixture subset.

use std::{collections::HashMap, future::Future, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};

#[derive(Clone, Debug)]
pub struct Request {
    pub target: String,
    pub headers: HashMap<String, String>,
}

pub struct Response {
    pub status: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub body_gate: Option<Arc<Semaphore>>,
}

impl Response {
    pub fn html(body: &str) -> Self {
        Self {
            status: "200 OK",
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: body.as_bytes().to_vec(),
            body_gate: None,
        }
    }
}

pub struct HttpFixture {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl HttpFixture {
    pub async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let handler = handler.clone();
                        clients.spawn(async move {
                            let mut bytes = Vec::new();
                            let mut buffer = [0; 4096];
                            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                                let n = socket.read(&mut buffer).await.unwrap_or(0);
                                if n == 0 { return; }
                                bytes.extend_from_slice(&buffer[..n]);
                                assert!(bytes.len() < 65536, "fixture request too large");
                            }
                            let text = String::from_utf8_lossy(&bytes);
                            let target = text.split_whitespace().nth(1).unwrap().to_owned();
                            let headers = text.split("\r\n").skip(1).filter_map(|line| {
                                line.split_once(':').map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                            }).collect();
                            let response = handler(Request { target, headers }).await;
                            let mut headers = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, response.body.len());
                            for (name, value) in response.headers { headers.push_str(&format!("{name}: {value}\r\n")); }
                            headers.push_str("\r\n");
                            if socket.write_all(headers.as_bytes()).await.is_err() { return; }
                            if let Some(gate) = response.body_gate {
                                let permit = gate.acquire().await.unwrap();
                                permit.forget();
                            }
                            let _ = socket.write_all(&response.body).await; // An aborted browser request may disconnect.
                        });
                    },
                    completed = clients.join_next(), if !clients.is_empty() => { completed.unwrap().unwrap(); },
                }
            }
        });
        Self { url, task }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn browser() -> eoka::Browser {
    eoka::Browser::launch_with(|c| {
        c.patch_binary = false;
        c.strip_x_client_data = false;
        c.cdp_timeout = 5;
    })
    .await
    .unwrap()
}
