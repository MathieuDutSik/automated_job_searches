use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

const USER_AGENT: &str = concat!(
    "ajs/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/automated_job_searches)"
);

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .gzip(true)
        .brotli(true)
        .build()?)
}
