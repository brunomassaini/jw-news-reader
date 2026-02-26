use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    pub url: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ImageInfo {
    pub url: String,
    pub alt: Option<String>,
    pub caption: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExtractResponse {
    pub markdown: String,
    pub title: Option<String>,
    pub source_url: String,
    pub images: Vec<ImageInfo>,
}
