// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::server_fsm::AnyServer;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::Idle;
use crate::relay::server_fsm::Server;
use crate::utils::hex_dump;

// The wire protocol pins the turn length, so the FSM must tick on this clock.
const TURN_LENGTH_MS: u32 = 200;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

// The ENet thread feeds decoded events in over `event_rx` and takes effects out
// over `_send_tx`. Those channels stay wired even though the FSM handlers do
// not exist yet, so filling them in later does not move the call sites.
pub fn run_game_server(
    event_rx: Receiver<InboundNetworkMessage>,
    _send_tx: Sender<OutboundNetworkMessage>,
    _shutdown_requested: Arc<AtomicBool>,
) {
    let config = Config {
        enabled_mods: Vec::new(),
        lobby_mode: false,
        turn_length_ms: TURN_LENGTH_MS,
    };

    // Parked in the listening state so the handler work starts from a live
    // server; until then events are only logged and never fed to the FSM.
    let _server: AnyServer = Server::<Idle>::new(config).listen().into();

    loop {
        loop {
            match event_rx.try_recv() {
                Ok(InboundNetworkMessage::Connect { peer, addr }) => {
                    tracing::debug!(?peer, %addr, "peer connected");
                }
                Ok(InboundNetworkMessage::Disconnect { peer, reason }) => {
                    tracing::debug!(?peer, reason, "peer disconnected");
                }
                Ok(InboundNetworkMessage::Message { peer, data }) => {
                    tracing::debug!(?peer, bytes = data.len(), payload = %hex_dump(&data), "message");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The ENet thread dropped its sender, which is how a
                    // deliberate shutdown reaches this loop.
                    tracing::info!("ENet event channel disconnected, shutting down");
                    return;
                }
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
