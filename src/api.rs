use crate::batcher::BatchSender;
use crate::error::ProxyError;
use actix_web::{HttpResponse, Responder, get, post, web};
use serde::Deserialize;

#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().body("ok")
}

#[derive(Deserialize)]
struct EmbedReq {
    input: String,
}

#[post("/embed")]
async fn embed(upstream: web::Data<BatchSender>, body: web::Json<EmbedReq>) -> Result<impl Responder, ProxyError> {
    let embedding = upstream.request(body.into_inner().input).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "embedding": embedding })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppConfig;
    use crate::batcher::{BatchItem, Batcher};
    use actix_web::{App, test};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // Helper: build a BatchSender that always returns a fixed embedding
    async fn test_sender_with_embedding(emb: Vec<f32>) -> BatchSender {
        let (tx, mut rx) = mpsc::channel::<BatchItem>(16);
        // Background task that replies to every BatchItem
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let _ = item.resp.send(Ok(emb.clone()));
            }
        });
        BatchSender::new(tx)
    }

    #[actix_web::test]
    async fn health_ok() {
        let app = test::init_service(App::new().service(health)).await;
        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::OK);

        let body = test::read_body(resp).await;
        assert_eq!(body, "ok");
    }

    #[actix_web::test]
    async fn embed_ok() {
        let sender = test_sender_with_embedding(vec![1.0, 2.0, 3.5]).await;

        let app = test::init_service(App::new().app_data(web::Data::new(sender)).service(embed)).await;

        let req = test::TestRequest::post()
            .uri("/embed")
            .set_json(serde_json::json!({ "input": "hello" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["embedding"], serde_json::json!([1.0, 2.0, 3.5]));
    }

    #[actix_web::test]
    async fn embed_upstream_ok() {
        let cfg = AppConfig::default();
        let (tx, rx) = mpsc::channel::<BatchItem>(cfg.queue_cap);
        let upstream = Arc::new(BatchSender::new(tx));
        Batcher::new(&cfg, rx).run(); // run batcher

        let app = test::init_service(App::new().app_data(web::Data::from(upstream.clone())).service(embed)).await;

        let req = test::TestRequest::post()
            .uri("/embed")
            .set_json(serde_json::json!({ "input": "hello" }))
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(resp).await;
        let emb = body["embedding"].as_array().expect("embedding should be an array");

        assert!(!emb.is_empty(), "embedding should be non-empty");
        assert!(emb.iter().all(|v| v.as_f64().is_some()), "elements should be numbers");
    }

    #[actix_web::test]
    async fn embed_503_when_batcher_unavailable() {
        // Create a sender and immediately drop the receiver to simulate crash/stop
        let (tx, _rx) = mpsc::channel::<BatchItem>(1);
        drop(_rx); // channel closed => send will error in BatchSender::request
        let sender = BatchSender::new(tx);

        let app = test::init_service(App::new().app_data(web::Data::new(sender)).service(embed)).await;

        let req = test::TestRequest::post()
            .uri("/embed")
            .set_json(serde_json::json!({ "input": "hello" }))
            .to_request();

        let resp = test::call_service(&app, req).await;

        // If BatchSender::request maps send error to ProxyError::BatcherUnavailable,
        // ResponseError should map it to 503.
        assert_eq!(resp.status(), actix_web::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
