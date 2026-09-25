use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url};

use super::{Transport, MAX_REPLY, MAX_REQUEST};
use crate::Error;

/// HTTP transport to the local QR bridge on the online computer.
pub struct HttpTransport {
    client: Client,
    endpoint: Url,
    session: String,
}

impl HttpTransport {
    /// Check bridge availability without asking the offline device to scan.
    /// The transport stays bound to this bridge session; reconnect explicitly
    /// after a restart or signer change.
    pub async fn connect(endpoint: &str) -> Result<Self, Error> {
        let endpoint = Url::parse(endpoint)
            .map_err(|_| Error::InvalidParameter("url", "Invalid bridge URL".into()))?;
        if endpoint.scheme() != "http"
            || endpoint.host_str() != Some("127.0.0.1")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || endpoint.query().is_some()
        {
            return Err(Error::InvalidParameter(
                "url",
                "Expected HTTP on 127.0.0.1 without credentials, query or fragment".into(),
            ));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| Error::Device(e.to_string()))?;
        let mut info = endpoint.clone();
        info.set_path("/info");
        let response = client
            .get(info)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| Error::Device(e.to_string()))?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Err(Error::DeviceDisconnected);
        }
        let session = response
            .headers()
            .get("x-thunderden-session")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() == 32 && value.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or(Error::Unexpected("Missing or invalid bridge session"))?
            .to_string();
        const MARKER: &str = "thunderden-qr-bridge";
        if response.status() != StatusCode::OK
            || response.content_length() != Some(MARKER.len() as u64)
            || response
                .text()
                .await
                .map_err(|e| Error::Device(e.to_string()))?
                != MARKER
        {
            return Err(Error::Unexpected("Not a Thunder Den QR bridge"));
        }
        Ok(Self {
            client,
            endpoint,
            session,
        })
    }

    /// Public ID for this bridge session; changes when the bridge restarts.
    pub fn session_id(&self) -> &str {
        &self.session
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn exchange(&self, request: &[u8]) -> Result<Vec<u8>, Error> {
        if request.len() > MAX_REQUEST {
            return Err(Error::UnsupportedInput);
        }
        // Allow time for manual QR scans and approval. Never retry a request automatically.
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .header("Content-Type", "application/cbor")
            .header("X-Thunderden-Session", &self.session)
            .body(request.to_vec())
            .send()
            .await
            .map_err(|e| Error::Device(e.to_string()))?;
        if response.status() == StatusCode::PRECONDITION_FAILED
            || response
                .headers()
                .get("x-thunderden-session")
                .and_then(|v| v.to_str().ok())
                != Some(self.session.as_str())
        {
            return Err(Error::DeviceDisconnected);
        }
        match response.status() {
            StatusCode::OK => {}
            StatusCode::GONE => return Err(Error::UserRefused),
            StatusCode::CONFLICT => {
                return Err(Error::Device(
                    "QR bridge busy; finish or cancel the current scan".into(),
                ))
            }
            _ => {
                return Err(Error::Device(
                    "QR bridge request failed; delivery may be uncertain".into(),
                ))
            }
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Error::Device(e.to_string()))?
        {
            if chunk.len() > MAX_REPLY - bytes.len() {
                return Err(Error::Unexpected("QR bridge reply too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
