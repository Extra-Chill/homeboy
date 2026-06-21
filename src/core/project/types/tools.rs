use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub bandcamp_scraper: BandcampScraperConfig,
    #[serde(default)]
    pub newsletter: NewsletterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BandcampScraperConfig {
    #[serde(default)]
    pub default_tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewsletterConfig {
    #[serde(default)]
    pub sendy_list_id: String,
}
