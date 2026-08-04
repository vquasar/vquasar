//! Serial-console WebSocket proxy (design document, section 25).
//!
//! Bridges a browser WebSocket to the owning host agent's `VmConsole` gRPC
//! stream:  browser  <—WS—>  ch-control  <—gRPC—>  ch-agent  <—>  VM serial.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use ch_proto::agent::ConsoleClientMessage;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;
use uuid::Uuid;

use crate::store::Store;

/// `GET /api/v1/vms/{id}/console` — upgrade to a WebSocket console session.
pub async fn console_ws(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
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
