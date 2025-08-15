mod api;
mod batcher;
mod error;

use crate::batcher::{BatchSender, Batcher};
use actix_web::{App, HttpServer, web};
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct AppConfig {
    pub bind_addr: String,
    pub tei_url: String,
    pub max_wait_time: u64,
    pub max_batch_size: usize,
    pub batch_concurrency: usize,
    pub queue_cap: usize,
    pub enqueue_timeout_ms: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
        let tei_url = env::var("TEI_URL").unwrap_or_else(|_| "http://tei:80".into());
        let max_wait_time = env::var("MAX_WAIT_TIME_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        let max_batch_size = env::var("MAX_BATCH_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32);
        let batch_concurrency = env::var("BATCH_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);
        let queue_cap = env::var("QUEUE_CAP").ok().and_then(|s| s.parse().ok()).unwrap_or(2048);
        let enqueue_timeout_ms = env::var("ENQUEUE_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(75);

        Self {
            bind_addr,
            tei_url,
            max_wait_time,
            max_batch_size,
            batch_concurrency,
            queue_cap,
            enqueue_timeout_ms,
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let cfg = AppConfig::default();
    let (tx, rx) = mpsc::channel::<batcher::BatchItem>(cfg.queue_cap);
    let upstream = Arc::new(BatchSender::new(tx));

    Batcher::new(&cfg, rx).run(); // run batcher

    // Server
    tracing::info!(
        "starting proxy on {} → TEI {} (wait={}ms, max_batch={})",
        cfg.bind_addr,
        cfg.tei_url,
        cfg.max_wait_time,
        cfg.max_batch_size
    );

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::from(upstream.clone()))
            .service(api::health)
            .service(api::embed)
    })
    .bind(cfg.bind_addr)?
    .run()
    .await
}
