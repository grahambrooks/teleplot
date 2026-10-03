//! Calls to the Teleplot server's JSON API, from the browser.

use serde::Serialize;
use serde::de::DeserializeOwned;

#[cfg(feature = "hydrate")]
pub async fn get<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let response = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    read(response).await
}

#[cfg(feature = "hydrate")]
pub async fn post<B: Serialize, T: DeserializeOwned>(url: &str, body: &B) -> Result<T, String> {
    let response = gloo_net::http::Request::post(url)
        .json(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    read(response).await
}

#[cfg(feature = "hydrate")]
async fn read<T: DeserializeOwned>(response: gloo_net::http::Response) -> Result<T, String> {
    if response.ok() {
        return response.json().await.map_err(|e| e.to_string());
    }
    match response.json::<crate::model::api::ApiError>().await {
        Ok(e) => Err(e.error),
        Err(_) => Err(format!("{} {}", response.status(), response.status_text())),
    }
}

// The server renders loading states and never fetches: resources that call
// these only run in the browser.
#[cfg(not(feature = "hydrate"))]
pub async fn get<T: DeserializeOwned>(_url: &str) -> Result<T, String> {
    Err("not available during server rendering".into())
}

#[cfg(not(feature = "hydrate"))]
pub async fn post<B: Serialize, T: DeserializeOwned>(_url: &str, _body: &B) -> Result<T, String> {
    Err("not available during server rendering".into())
}

/// Percent-encodes a URL path segment or query value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
