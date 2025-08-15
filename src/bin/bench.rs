use bytes::Bytes;
use clap::{Parser, ValueEnum};
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use tokio::sync::Semaphore;

/// Where to send requests
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Service {
    Proxy,
    Native,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Number of requests to send.
    #[arg(short, long)]
    requests: usize,

    #[arg(short, long)]
    concurrency: usize,

    #[arg(short, long, value_enum)]
    service: Service,

    /// Approximate tokens per request (repeated word tokens).
    /// Use realistic sizes like 32, 128, 256, 512…
    #[arg(short, long, default_value_t = 128)]
    tokens: usize,
}

fn make_base_text(tokens: usize) -> String {
    // Roughly 1 token per word for common tokenizers; good enough for load gen.
    let mut s = String::with_capacity(tokens * 5);
    for _ in 0..tokens {
        s.push_str("word ");
    }
    s
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let total = args.requests;
    let conc = args.concurrency;
    let mode = args.service;

    let url = match mode {
        Service::Proxy => "http://localhost:3000/embed",
        Service::Native => "http://localhost:8080/embed",
    };

    // Prebuild one ~N-token text and JSON bodies to avoid client-side CPU overhead.
    let base_text = Arc::new(make_base_text(args.tokens));
    let body_proxy: Bytes = Bytes::from(serde_json::to_vec(&serde_json::json!({ "input":  &*base_text })).unwrap());
    let body_native: Bytes = Bytes::from(serde_json::to_vec(&serde_json::json!({ "inputs": [&*base_text] })).unwrap());

    let client = Arc::new(Client::builder().build().unwrap());
    let sem = Arc::new(Semaphore::new(conc));
    let lat_ptr = Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(total)));
    let successes = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(AtomicUsize::new(0));

    let started = Instant::now();
    let mut handles = Vec::with_capacity(total);

    for _ in 0..total {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let lat_ptr = lat_ptr.clone();
        let client = client.clone();
        let successes = successes.clone();
        let failures = failures.clone();
        let body_proxy = body_proxy.clone();
        let body_native = body_native.clone();

        handles.push(tokio::spawn(async move {
            let _p = permit;
            let body = match mode {
                Service::Proxy => body_proxy,
                Service::Native => body_native,
            };
            let t0 = Instant::now();
            let res = client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await;
            let elapsed = t0.elapsed();

            match res {
                Ok(r) => {
                    if r.status().is_success() {
                        successes.fetch_add(1, Ordering::Relaxed);
                    } else {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                    // Drain body to keep connections healthy
                    let _ = r.bytes().await;
                }
                Err(_) => {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
            }

            lat_ptr.lock().await.push(elapsed);
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = started.elapsed();
    let mut lats = Arc::try_unwrap(lat_ptr).unwrap().into_inner();
    lats.sort_unstable();

    let pick = |pp: f64| lats[(lats.len() as f64 * pp).min(lats.len() as f64 - 1.0) as usize];

    println!("throughput: {:.1} req/s", total as f64 / elapsed.as_secs_f64());
    println!(
        "successes: {}  fail: {}",
        successes.load(Ordering::Relaxed),
        failures.load(Ordering::Relaxed)
    );
    println!("latency p50: {:.2} ms", pick(0.50).as_millis());
    println!("latency p95: {:.2} ms", pick(0.95).as_millis());
    println!("latency p99: {:.2} ms", pick(0.99).as_millis());
}
