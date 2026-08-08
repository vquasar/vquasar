//! Control-plane identity check on the agent's gRPC server (design §30).
//!
//! Mutual TLS on its own only proves the peer holds *a* certificate signed by
//! our CA. Every agent has one of those. Since the agent is the only privileged
//! component and its API can create, delete and open a console on any VM, a
//! peer that is merely CA-signed is not good enough: one compromised host could
//! otherwise drive every other host, turning a host compromise into a fleet
//! compromise — exactly what §30 says must not happen.
//!
//! So the agent additionally requires the peer certificate's Common Name to be
//! the configured control-plane identity.
//!
//! It also asks a second question here, and only here: is this the *current*
//! control plane? Every instance presents the same CN by design (ADR-021), so
//! the certificate cannot distinguish a leader from one that has been
//! superseded. The lease epoch can, and this is the right place to check it —
//! the same kind of question, asked at the same point, in front of all thirteen
//! RPCs rather than repeated in each of them (ADR-022). The comparison itself
//! lives in [`crate::epoch`].

// tonic's `Status` is a large error type used pervasively by the generated
// trait; boxing every return would fight the API for no benefit.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use tonic::{Request, Status};
use x509_parser::prelude::*;

use crate::epoch::EpochGuard;

/// Reject any request that is not from the control plane, or is from one that
/// has been superseded.
///
/// Used as a tonic interceptor, so the checks run before any RPC handler. When
/// TLS is not configured there are no peer certificates and the identity check
/// is a no-op — a plaintext agent has no identity to verify, which is why
/// plaintext is only for a trusted lab (the startup log says so).
#[derive(Clone)]
pub struct RequireControlIdentity {
    expected_cn: Option<String>,
    /// `None` leaves epoch fencing off entirely, which is what a plaintext
    /// agent gets: with no identity to bind it to, an epoch is a number any
    /// caller can choose, and enforcing it would suggest a guarantee that is
    /// not there.
    epoch: Option<Arc<EpochGuard>>,
}

impl RequireControlIdentity {
    /// `expected_cn` is `None` when mTLS is off, disabling the check.
    pub fn new(expected_cn: Option<String>) -> Self {
        Self {
            expected_cn,
            epoch: None,
        }
    }

    /// Also refuse controllers whose lease epoch is behind one already seen.
    pub fn with_epoch(mut self, guard: Arc<EpochGuard>) -> Self {
        self.epoch = Some(guard);
        self
    }
}

impl tonic::service::Interceptor for RequireControlIdentity {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        // Identity first: an epoch from an unidentified caller means nothing,
        // and rejecting on it would report the wrong reason.
        self.check_identity(&req)?;
        if let Some(guard) = &self.epoch {
            guard.check(req.metadata())?;
        }
        Ok(req)
    }
}

impl RequireControlIdentity {
    fn check_identity(&self, req: &Request<()>) -> Result<(), Status> {
        let Some(expected) = &self.expected_cn else {
            return Ok(());
        };
        // With mTLS configured, tonic will already have rejected anything that
        // does not chain to our CA; absence of a certificate here would mean
        // the TLS config was not applied, so fail closed.
        let certs = req
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let leaf = certs
            .first()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let (_, parsed) = X509Certificate::from_der(leaf.as_ref())
            .map_err(|_| Status::unauthenticated("unparsable client certificate"))?;
        let cn = common_name(&parsed);
        match cn.as_deref() {
            Some(cn) if cn == expected => Ok(()),
            other => {
                // Name the mismatch: the usual cause is a control certificate
                // issued with a different CN, and the operator needs to know
                // which value to configure.
                tracing::warn!(
                    peer_cn = other.unwrap_or("<none>"),
                    expected = %expected,
                    "rejected gRPC peer: not the control plane (set [tls] control_cn if this is wrong)"
                );
                Err(Status::permission_denied(
                    "client certificate is not the control plane",
                ))
            }
        }
    }
}

/// The certificate subject's Common Name, if it has one.
fn common_name(cert: &X509Certificate<'_>) -> Option<String> {
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tonic::service::Interceptor;

    fn have_openssl() -> bool {
        Command::new("openssl")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!(
            "vquasar-peerid-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn openssl(args: &[&str]) {
        let out = Command::new("openssl").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A CA plus one leaf per name, mirroring `scripts/gen-certs.sh`. Every
    /// leaf is signed by the same CA — which is exactly why chaining to the CA
    /// cannot serve as the identity check.
    fn pki(dir: &Path, names: &[&str]) {
        let p = |f: String| dir.join(f).to_string_lossy().into_owned();
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            &p("ca.key".into()),
            "-out",
            &p("ca.crt".into()),
            "-days",
            "1",
            "-subj",
            "/CN=vquasar-ca",
        ]);
        for name in names {
            openssl(&[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &p(format!("{name}.key")),
                "-out",
                &p(format!("{name}.csr")),
                "-subj",
                &format!("/CN={name}"),
            ]);
            std::fs::write(
                dir.join(format!("{name}.ext")),
                "subjectAltName = DNS:localhost,IP:127.0.0.1\n\
                 extendedKeyUsage = serverAuth, clientAuth\n",
            )
            .unwrap();
            openssl(&[
                "x509",
                "-req",
                "-in",
                &p(format!("{name}.csr")),
                "-CA",
                &p("ca.crt".into()),
                "-CAkey",
                &p("ca.key".into()),
                "-CAcreateserial",
                "-out",
                &p(format!("{name}.crt")),
                "-days",
                "1",
                "-extfile",
                &p(format!("{name}.ext")),
            ]);
        }
    }

    fn cn_of_cert(dir: &Path, name: &str) -> Option<String> {
        let pem = std::fs::read(dir.join(format!("{name}.crt"))).unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
        let cert = pem.parse_x509().unwrap();
        common_name(&cert)
    }

    #[test]
    fn reads_the_common_name_from_a_certificate() {
        if !have_openssl() {
            eprintln!("skipping: openssl not available");
            return;
        }
        let dir = scratch("cn");
        pki(&dir, &["control", "agent-host02"]);
        assert_eq!(cn_of_cert(&dir, "control").as_deref(), Some("control"));
        // An agent's certificate is not the control plane, though the CA signed both.
        assert_eq!(
            cn_of_cert(&dir, "agent-host02").as_deref(),
            Some("agent-host02")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_check_is_disabled_when_tls_is_off() {
        let mut i = RequireControlIdentity::new(None);
        assert!(i.call(Request::new(())).is_ok());
    }

    /// With mTLS on, a request carrying no peer certificate is refused rather
    /// than waved through.
    #[test]
    fn no_peer_certificate_fails_closed() {
        let mut i = RequireControlIdentity::new(Some("control".to_string()));
        let err = i.call(Request::new(())).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    /// The wiring, which is where the defect lived: mutual TLS was configured
    /// correctly and still admitted any CA-signed certificate. Stand up a real
    /// tonic server with mTLS and dial it with two different identities.
    #[tokio::test]
    async fn only_the_control_certificate_reaches_the_service() {
        use tonic::transport::{Certificate, ClientTlsConfig, Identity, Server, ServerTlsConfig};
        use vquasar_proto::agent::host_agent_client::HostAgentClient;
        use vquasar_proto::agent::host_agent_server::HostAgentServer;

        if !have_openssl() {
            eprintln!("skipping: openssl not available");
            return;
        }
        // rustls 0.23 needs a process-wide provider before any TLS use, as
        // main() does for the real server.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = scratch("it");
        pki(&dir, &["agent-host01", "control", "agent-host02"]);
        let state = tempfile::tempdir().unwrap();
        let svc = crate::grpc::tests::service(state.path());
        let ident = |name: &str| {
            Identity::from_pem(
                std::fs::read(dir.join(format!("{name}.crt"))).unwrap(),
                std::fs::read(dir.join(format!("{name}.key"))).unwrap(),
            )
        };
        let ca = || Certificate::from_pem(std::fs::read(dir.join("ca.crt")).unwrap());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();

        let tls = ServerTlsConfig::new()
            .identity(ident("agent-host01"))
            .client_ca_root(ca());
        let server = tokio::spawn(async move {
            Server::builder()
                .tls_config(tls)
                .unwrap()
                .add_service(HostAgentServer::with_interceptor(
                    svc,
                    RequireControlIdentity::new(Some("control".to_string())),
                ))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
        });

        async fn dial(
            addr: &str,
            ca: Certificate,
            id: Identity,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let tls = ClientTlsConfig::new()
                .ca_certificate(ca)
                .identity(id)
                .domain_name("localhost");
            let channel = tonic::transport::Channel::from_shared(format!("https://{addr}"))?
                .tls_config(tls)?
                .connect()
                .await?;
            HostAgentClient::new(channel)
                .get_host_info(vquasar_proto::agent::GetHostInfoRequest::default())
                .await?;
            Ok(())
        }

        let ok = dial(&addr, ca(), ident("control")).await;
        assert!(ok.is_ok(), "control must be admitted: {ok:?}");

        // A peer host's own certificate is CA-signed and must still be refused.
        let denied = dial(&addr, ca(), ident("agent-host02")).await;
        let msg = format!("{:?}", denied.expect_err("agent cert must be refused"));
        assert!(
            msg.contains("PermissionDenied") || msg.contains("not the control plane"),
            "unexpected error: {msg}"
        );

        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
