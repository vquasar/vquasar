//! Token-based agent auto-enrollment (design M16).
//!
//! Because control *dials* agents (the agent is a pure mTLS server), a new
//! agent must obtain its certificate before control can reach it — so
//! enrollment is agent-initiated:
//!
//! 1. An operator calls [`enroll`] (RBAC-guarded) with the new host's name +
//!    agent endpoint. Control registers the host and mints a one-time, TTL'd
//!    token (only its SHA-256 hash is stored).
//! 2. On the new host, the installer generates a keypair + CSR and POSTs the CSR
//!    to [`bootstrap_sign`], authenticating with the token over server-TLS
//!    (trusting control via the root CA it already has).
//! 3. Control validates the token, signs the CSR with the **intermediate**
//!    issuing CA (setting the SAN itself from the enrolled endpoint, so a token
//!    can't mint an arbitrary identity), and returns the leaf+intermediate
//!    chain. The agent writes it and starts serving mTLS; the reconcile loop
//!    then dials it and flips it Ready.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::api::error::{ApiError, ApiResult};
use crate::authz::RequireHostManage;
use crate::store::Store;

/// Paths + settings the enrollment endpoints need, injected as an extension.
#[derive(Clone)]
pub struct EnrollmentState {
    /// Root CA cert path (returned to operators; the trust anchor).
    pub root_ca: String,
    /// Intermediate issuing-CA cert + key paths (signer).
    pub issuer_cert: String,
    pub issuer_key: String,
    /// HTTPS URL agents reach control at (for the returned bootstrap command).
    pub control_url: Option<String>,
    pub token_ttl_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
    /// Agent gRPC endpoint control will dial, e.g. `http://host.lab:9500`.
    pub endpoint: String,
}

#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub host_id: String,
    /// The one-time bootstrap token (shown once).
    pub token: String,
    /// URL the agent posts its CSR to (None if control_url is unconfigured).
    pub bootstrap_url: Option<String>,
    /// Root CA (PEM) the agent must trust to reach control during bootstrap.
    pub ca_cert: String,
    pub expires_in_secs: u64,
}

fn hash_token(token: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// Register a host and mint a one-time enrollment token (design M16).
pub async fn enroll(
    State(store): State<Store>,
    _: RequireHostManage,
    Extension(en): Extension<EnrollmentState>,
    Json(body): Json<EnrollRequest>,
) -> ApiResult<Json<EnrollResponse>> {
    if body.name.is_empty() || body.endpoint.is_empty() {
        return Err(ApiError::invalid("name and endpoint are required"));
    }
    let host = store.register_host(&body.name, &body.endpoint).await?;

    // 32 bytes of CSPRNG entropy, base64url; store only its hash.
    let mut raw = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw)
        .map_err(|_| ApiError::internal("failed to generate token"))?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let expires = chrono::Utc::now() + chrono::Duration::seconds(en.token_ttl_secs as i64);
    store
        .insert_enrollment_token(host.id, &hash_token(&token), expires)
        .await?;

    let ca_cert = tokio::fs::read_to_string(&en.root_ca)
        .await
        .map_err(|e| ApiError::internal(format!("read CA cert: {e}")))?;
    store
        .insert_event("host", Some(host.id), "host.enroll", "info", &host.name)
        .await?;

    Ok(Json(EnrollResponse {
        host_id: host.id.to_string(),
        token,
        bootstrap_url: en
            .control_url
            .as_ref()
            .map(|u| format!("{}/api/v1/enroll/sign", u.trim_end_matches('/'))),
        ca_cert,
        expires_in_secs: en.token_ttl_secs,
    }))
}

/// Sign an agent CSR presented with a valid enrollment token (design M16). The
/// token is taken from the `X-Enrollment-Token` header; the request body is the
/// CSR (PEM). Returns the leaf+intermediate chain as `text/plain` so a plain
/// `curl` on the agent can write it straight to the cert file. No OIDC — the
/// one-time token is the sole authenticator (this runs before the agent has a
/// client cert).
pub async fn bootstrap_sign(
    State(store): State<Store>,
    Extension(en): Extension<EnrollmentState>,
    headers: HeaderMap,
    csr_pem: String,
) -> Result<axum::response::Response, ApiError> {
    let token = headers
        .get("x-enrollment-token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::invalid("missing X-Enrollment-Token header"))?;

    let host_id = store
        .consume_enrollment_token(&hash_token(token))
        .await?
        .ok_or_else(|| {
            ApiError::unauthorized("invalid, expired, or already-used enrollment token")
        })?;
    let host = store
        .get_host(host_id)
        .await?
        .ok_or_else(|| ApiError::internal("enrolled host vanished"))?;

    // Control chooses the identity from the enrolled endpoint — never from the
    // CSR — so a token can only ever yield a cert for the host it enrolled.
    let san = san_for_endpoint(&host.endpoint)
        .ok_or_else(|| ApiError::invalid("cannot derive SAN from host endpoint"))?;

    let chain = sign_csr(csr_pem, en.issuer_cert.clone(), en.issuer_key.clone(), san)
        .await
        .map_err(|e| ApiError::internal(format!("sign CSR: {e}")))?;

    store
        .insert_event(
            "host",
            Some(host_id),
            "host.enroll.signed",
            "info",
            &host.name,
        )
        .await?;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-pem-file")],
        chain,
    )
        .into_response())
}

/// `http://chnode3.lab.k8:9500` -> `DNS:chnode3.lab.k8` (or `IP:…`).
fn san_for_endpoint(endpoint: &str) -> Option<String> {
    let after_scheme = endpoint.split("://").last()?;
    let hostport = after_scheme.split('/').next()?;
    // Strip the port (host is everything before the last ':' when a port is present).
    let host = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => h,
        _ => hostport,
    };
    if host.is_empty() {
        return None;
    }
    let is_ipv4 = host.split('.').count() == 4 && host.split('.').all(|o| o.parse::<u8>().is_ok());
    Some(if is_ipv4 {
        format!("IP:{host}")
    } else {
        format!("DNS:{host}")
    })
}

/// Sign a CSR with the intermediate CA via openssl (consistent with
/// `gen-certs.sh`), returning the leaf+intermediate PEM chain.
async fn sign_csr(
    csr_pem: String,
    issuer_cert: String,
    issuer_key: String,
    san: String,
) -> std::io::Result<String> {
    tokio::task::spawn_blocking(move || {
        use std::io::{Error, ErrorKind};
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!("ch-enroll-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir)?;
        let cleanup = || {
            let _ = std::fs::remove_dir_all(&dir);
        };
        let run = |result: std::io::Result<String>| -> std::io::Result<String> {
            if result.is_err() {
                cleanup();
            }
            result
        };

        let csr_path = dir.join("agent.csr");
        let ext_path = dir.join("agent.ext");
        let leaf_path = dir.join("agent.crt");
        let srl_path = dir.join("int.srl");
        std::fs::write(&csr_path, &csr_pem)?;
        std::fs::write(
            &ext_path,
            format!(
                "subjectAltName = {san}\n\
                 extendedKeyUsage = serverAuth, clientAuth\n\
                 keyUsage = digitalSignature, keyEncipherment\n"
            ),
        )?;

        // Proof of possession: reject a CSR whose signature doesn't verify.
        let verify = Command::new("openssl")
            .args(["req", "-in"])
            .arg(&csr_path)
            .args(["-verify", "-noout"])
            .output()?;
        if !verify.status.success() {
            return run(Err(Error::new(
                ErrorKind::InvalidData,
                "CSR failed verification",
            )));
        }

        let out = Command::new("openssl")
            .args(["x509", "-req", "-in"])
            .arg(&csr_path)
            .args(["-CA"])
            .arg(&issuer_cert)
            .args(["-CAkey"])
            .arg(&issuer_key)
            .args(["-CAserial"])
            .arg(&srl_path)
            .arg("-CAcreateserial")
            .args(["-sha256", "-days", "825", "-out"])
            .arg(&leaf_path)
            .args(["-extfile"])
            .arg(&ext_path)
            .output()?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).to_string();
            return run(Err(Error::other(msg)));
        }

        let leaf = std::fs::read_to_string(&leaf_path)?;
        let intermediate = std::fs::read_to_string(&issuer_cert)?;
        cleanup();
        // Chain: leaf first, then the intermediate, so control (trusting the
        // root) can build leaf -> intermediate -> root.
        Ok(format!("{}\n{}", leaf.trim_end(), intermediate.trim_end()))
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::san_for_endpoint;

    #[test]
    fn san_from_dns_endpoint() {
        assert_eq!(
            san_for_endpoint("http://chnode3.lab.k8:9500").as_deref(),
            Some("DNS:chnode3.lab.k8")
        );
    }

    #[test]
    fn san_from_ipv4_endpoint() {
        assert_eq!(
            san_for_endpoint("https://172.16.56.83:9500").as_deref(),
            Some("IP:172.16.56.83")
        );
    }

    #[test]
    fn san_without_scheme_or_port() {
        assert_eq!(
            san_for_endpoint("host.example").as_deref(),
            Some("DNS:host.example")
        );
    }

    #[test]
    fn san_rejects_empty() {
        assert_eq!(san_for_endpoint("http://"), None);
    }
}
