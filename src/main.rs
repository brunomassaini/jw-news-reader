use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

mod extract;
mod models;

use models::{ExtractRequest, ExtractResponse};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = Router::new()
        .route("/health", get(health))
        .route("/extract", post(extract_endpoint));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    tracing::info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn extract_endpoint(Json(req): Json<ExtractRequest>) -> Response {
    match extract::extract_article(&req.url).await {
        Ok(result) => {
            let response = ExtractResponse {
                markdown: result.markdown,
                title: result.title,
                source_url: result.source_url,
                images: result.images,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            use extract::ExtractionError;
            let (status, detail) = match &e {
                ExtractionError::InvalidUrl(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
                ExtractionError::NotHtml => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "URL did not return HTML".to_string(),
                ),
                ExtractionError::Upstream => {
                    (StatusCode::BAD_GATEWAY, "Upstream returned an error".to_string())
                }
                ExtractionError::Request(msg) => (
                    StatusCode::BAD_GATEWAY,
                    format!("Upstream request failed: {}", msg),
                ),
            };
            (status, Json(json!({"detail": detail}))).into_response()
        }
    }
}
