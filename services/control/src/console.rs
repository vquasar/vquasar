//! Serial-console WebSocket proxy (design document, section 25).
//!
//! Bridges a browser WebSocket to the owning host agent's `VmConsole` gRPC
//! stream:  browser  <—WS—>  vquasar-control  <—gRPC—>  vquasar-agent  <—>  VM serial.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;
use uuid::Uuid;
use vquasar_proto::agent::ConsoleClientMessage;

use crate::authz::AuthState;
use crate::store::Store;

#[derive(Debug, Deserialize)]
pub struct ConsoleAuth {
    /// Browsers can't set WS auth headers, so the console token rides a query
    /// param and is validated here (design M12b), gated on `vm:console`.
    #[serde(default)]
    access_token: Option<String>,
}

/// `GET /api/v1/vms/{id}/console` — upgrade to a WebSocket console session.
pub async fn console_ws(
    State(store): State<Store>,
    Extension(auth): Extension<AuthState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ConsoleAuth>,
    ws: WebSocketUpgrade,
) -> Response {
    // Authenticate + authorize before upgrading, unless auth is disabled (dev).
    if let Some(authenticator) = auth.authenticator {
        let Some(token) = q.access_token else {
            return (StatusCode::UNAUTHORIZED, "missing access_token").into_response();
        };
        let claims = match authenticator.validate(&token).await {
            Ok(c) => c,
            Err(e) => return (StatusCode::UNAUTHORIZED, e.to_string()).into_response(),
        };
        let allowed = match store
            .upsert_user(
                &claims.subject,
                &claims.username,
                claims.email.as_deref(),
                claims.display_name.as_deref(),
            )
            .await
        {
            Ok(user) => store
                .effective_permissions(user.id, &claims.groups)
                .await
                .map(|p| p.contains("vm:console"))
                .unwrap_or(false),
            Err(_) => false,
        };
        if !allowed {
            return (StatusCode::FORBIDDEN, "missing permission: vm:console").into_response();
        }
    }
    ws.on_upgrade(move |socket| handle(store, id, socket))
}

async fn handle(store: Store, id: Uuid, socket: WebSocket) {
    // Resolve the VM's host and agent endpoint.
    let endpoint = match resolve_endpoint(&store, id).await {
        Some(e) => e,
        None => return,
    };

    let mut client = match crate::agent::connect_host_agent(&endpoint).await {
        Ok(c) => c,
        Err(e) => {
            debug!(vm = %id, error = %e, "console: cannot reach agent");
            return;
        }
    };

    // Outbound gRPC stream: first message selects the VM, then carries input.
    let (to_agent, rx) = mpsc::channel::<ConsoleClientMessage>(64);
    if to_agent
        .send(ConsoleClientMessage {
            vm_id: id.to_string(),
            input: Vec::new(),
        })
        .await
        .is_err()
    {
        return;
    }

    let mut inbound = match client.vm_console(ReceiverStream::new(rx)).await {
        Ok(resp) => resp.into_inner(),
        Err(e) => {
            debug!(vm = %id, error = %e, "console: VmConsole rpc failed");
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Guest serial output -> browser.
    let out = tokio::spawn(async move {
        while let Ok(Some(msg)) = inbound.message().await {
            if ws_tx.send(Message::Binary(msg.output)).await.is_err() {
                break;
            }
        }
    });

    // Browser keystrokes -> guest.
    while let Some(Ok(frame)) = ws_rx.next().await {
        let input = match frame {
            Message::Binary(b) => b,
            Message::Text(t) => t.into_bytes(),
            Message::Close(_) => break,
            _ => continue,
        };
        if to_agent
            .send(ConsoleClientMessage {
                vm_id: String::new(),
                input,
            })
            .await
            .is_err()
        {
            break;
        }
    }

    out.abort();
}

async fn resolve_endpoint(store: &Store, id: Uuid) -> Option<String> {
    let vm = store.get_vm(id).await.ok().flatten()?;
    let host_id = vm.host_id?;
    let host = store.get_host(host_id).await.ok().flatten()?;
    Some(host.endpoint)
}
