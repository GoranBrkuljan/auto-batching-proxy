use crate::AppConfig;
use crate::error::ProxyError;
use reqwest::Client;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::time::Instant;

pub struct BatchItem {
    pub input: String,
    pub resp: oneshot::Sender<Result<Vec<f32>, ProxyError>>,
}

/// Sends items to the batcher.
pub struct BatchSender {
    tx: mpsc::Sender<BatchItem>,
}

impl BatchSender {
    pub fn new(tx: mpsc::Sender<BatchItem>) -> Self {
        Self { tx }
    }

    /// Enqueue and await result
    pub async fn request(&self, input: String) -> Result<Vec<f32>, ProxyError> {
        let (tx_resp, rx_resp) = oneshot::channel();
        let item = BatchItem { input, resp: tx_resp };
        self.tx.send(item).await.map_err(|_| ProxyError::BatcherUnavailable)?;

        rx_resp.await?
    }
}

/// Receives, batches, and sends items to the upstream TEI service.
pub struct Batcher {
    rx: mpsc::Receiver<BatchItem>,
    client: Client,
    tei_url: String,
    max_wait_time: Duration,
    max_batch_size: usize,
    /// Limits number of concurrent requests to the TEI.
    inflight: Arc<Semaphore>,
}

impl Batcher {
    pub fn new(cfg: &AppConfig, rx: mpsc::Receiver<BatchItem>) -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(256)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .http1_only()
            .build()
            .expect("reqwest client");

        Self {
            rx,
            client,
            tei_url: cfg.tei_url.clone(),
            max_wait_time: Duration::from_millis(cfg.max_wait_time),
            max_batch_size: cfg.max_batch_size,
            inflight: Arc::new(Semaphore::new(cfg.batch_concurrency)),
        }
    }

    /// Spawn the accumulator loop. Each flush is executed in its own task.
    pub fn run(mut self) {
        tokio::spawn(async move {
            while let Some(batch) = self.receive_batch().await {
                self.send_batch(batch);
            }

            tracing::info!("batcher exiting: channel closed");
        });
    }

    /// Receives and accumulates batch items until `max_batch_size` or `max_wait_time` deadline is reached.
    async fn receive_batch(&mut self) -> Option<Vec<BatchItem>> {
        let first = self.rx.recv().await?;
        let mut batch = Vec::with_capacity(self.max_batch_size);
        batch.push(first);

        let deadline = Instant::now() + self.max_wait_time;

        loop {
            // Fast-drain whatever is already queued
            while batch.len() < self.max_batch_size {
                match self.rx.try_recv() {
                    Ok(item) => batch.push(item),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Some(batch),
                }
            }

            if batch.len() == self.max_batch_size {
                return Some(batch);
            }

            let now = Instant::now();
            let remaining = deadline - now;

            if now >= deadline {
                return Some(batch);
            }

            // Wait for more items or deadline. If multiple new items are present,
            // they will be processed in the next iteration. This way we can avoid busy-waiting.
            match tokio::time::timeout(remaining, self.rx.recv()).await {
                Ok(Some(item)) => {
                    batch.push(item);

                    if batch.len() == self.max_batch_size {
                        return Some(batch);
                    }
                }
                Ok(None) => return Some(batch), // closed; flush what we have
                Err(_) => return Some(batch),   // deadline reached
            }
        }
    }

    /// Sends batch to the upstream service with spawned task, so accumulator
    /// can immediately continue with subsequent items.
    fn send_batch(&mut self, batch: Vec<BatchItem>) {
        let client = self.client.clone();
        let embed_url = format!("{}/embed", self.tei_url);
        let inflight = self.inflight.clone();

        tokio::spawn(async move {
            let _permit = match inflight.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    for item in batch {
                        let _ = item.resp.send(Err(ProxyError::ServiceShutdown));
                    }

                    tracing::warn!("service shutting down");
                    return;
                }
            };

            #[derive(serde::Serialize)]
            struct EmbReq<'a> {
                inputs: Vec<&'a str>,
            }

            let req = EmbReq {
                inputs: batch.iter().map(|b| b.input.as_str()).collect(),
            };
            let resp = client.post(embed_url).json(&req).send().await;
            let result: Result<Vec<Vec<f32>>, ProxyError> = match resp {
                Ok(r) if r.status().is_success() => r.json().await.map_err(ProxyError::from),
                Ok(r) => {
                    let code = r.status().as_u16();
                    let body = r.text().await.unwrap_or_default();

                    Err(ProxyError::Upstream { code, body })
                }
                Err(e) => Err(ProxyError::from(e)),
            };

            match result {
                Ok(embs) if embs.len() == batch.len() => {
                    let item_count = batch.len();

                    for (item, emb) in batch.into_iter().zip(embs) {
                        let _ = item.resp.send(Ok(emb));
                    }

                    tracing::info!(batch = %item_count, "flush_ok");
                }
                Ok(embs) => {
                    let got = embs.len();
                    let exp = batch.len();

                    for item in batch {
                        let _ = item.resp.send(Err(ProxyError::CountMismatch { expected: exp, got }));
                    }

                    tracing::error!("embedding count mismatch: got {got}, expected {exp}");
                }
                Err(e) => {
                    for item in batch {
                        let _ = item.resp.send(Err(e.clone()));
                    }

                    tracing::error!(error = %e, "flush_err");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tokio::sync::mpsc;
    use tokio::time::Duration;

    // Small helper to build a Batcher with hand-picked params.
    fn mk_batcher(rx: mpsc::Receiver<BatchItem>, max_batch: usize, max_wait_ms: u64) -> Batcher {
        Batcher {
            rx,
            client: Client::builder().build().unwrap(),
            tei_url: env::var("TEI_URL").expect("TEI_URL must be set"),
            max_wait_time: Duration::from_millis(max_wait_ms),
            max_batch_size: max_batch,
            inflight: Arc::new(Semaphore::new(8)),
        }
    }

    #[tokio::test]
    async fn receive_batch_respects_size_cap() {
        let (tx, rx) = mpsc::channel::<BatchItem>(64);
        // Pre-fill > max_batch items quickly
        for i in 0..10 {
            let (txr, _rxr) = oneshot::channel();
            tx.send(BatchItem {
                input: format!("i-{i}"),
                resp: txr,
            })
            .await
            .unwrap();
        }

        let mut b = mk_batcher(rx, 4, 500);
        let batch = b.receive_batch().await.expect("some batch");
        assert_eq!(batch.len(), 4, "should flush exactly at max_batch_size");
    }

    #[tokio::test]
    async fn receive_batch_respects_timeout() {
        let (tx, rx) = mpsc::channel::<BatchItem>(64);

        // Send exactly one, then wait longer than max_wait
        let (txr, _rxr) = oneshot::channel();
        tx.send(BatchItem {
            input: "first".into(),
            resp: txr,
        })
        .await
        .unwrap();

        let mut b = mk_batcher(rx, 8, 30);
        let t0 = Instant::now();
        let batch = b.receive_batch().await.expect("some batch");
        let waited = t0.elapsed();

        assert_eq!(batch.len(), 1, "should flush partial when deadline hits");
        assert!(
            waited >= Duration::from_millis(30),
            "should wait ~max_wait before flushing"
        );
    }

    #[tokio::test]
    async fn send_batch_fans_out_error_to_all_waiters() {
        // Point upstream to an unroutable endpoint to force a Request error
        let (_tx, rx) = mpsc::channel::<BatchItem>(1);
        let mut b = mk_batcher(rx, 4, 10);
        b.tei_url = "http://127.0.0.1:12345".to_string();

        // Build a manual batch of 3 items with receivers we can await
        let mut rxs = Vec::new();
        let mut batch = Vec::new();
        for i in 0..3 {
            let (txr, rxr) = oneshot::channel();
            batch.push(BatchItem {
                input: format!("x-{i}"),
                resp: txr,
            });
            rxs.push(rxr);
        }

        // Call send_batch (spawns a task)
        b.send_batch(batch);

        // All receivers should error
        for rx in rxs {
            let err = rx.await.expect("oneshot should arrive").expect_err("should be Err");

            assert!(matches!(err, ProxyError::Request(_)));
        }
    }

    #[tokio::test]
    async fn receive_batch_then_channel_close_returns_none_next_time() {
        let (tx, rx) = mpsc::channel::<BatchItem>(4);
        // Send one item
        let (txr, _rxr) = oneshot::channel();
        tx.send(BatchItem {
            input: "one".into(),
            resp: txr,
        })
        .await
        .unwrap();
        drop(tx); // close channel

        let mut b = mk_batcher(rx, 4, 50);
        // First call: flush the single item
        let _ = b.receive_batch().await.expect("first batch");
        // Second call: should see channel closed before first receive → None
        assert!(b.receive_batch().await.is_none());
    }
}
