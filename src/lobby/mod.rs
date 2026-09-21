// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

pub mod auth;
pub mod connection_data;
pub mod gamelist;
pub mod link;

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use futures::stream::StreamExt;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tokio_xmpp::Client;
use tokio_xmpp::Stanza;
use tokio_xmpp::minidom::Element;
use tokio_xmpp::parsers::iq::Iq;
use tokio_xmpp::parsers::jid::Jid;
use tokio_xmpp::parsers::message::Message as XmppMessage;
use tokio_xmpp::parsers::message::MessageType;
use tokio_xmpp::parsers::muc::Muc;
use tokio_xmpp::parsers::presence::Presence;
use tokio_xmpp::parsers::presence::Type as PresenceType;
use tracing::Instrument;

use crate::lobby::connection_data::Assignment;
use crate::lobby::link::GameToLobby;
use crate::lobby::link::LobbyAuthToken;

// Spread out so the lobby server's TCP accept queue never sees every account
// connect in the same instant.
const ACCOUNT_SPAWN_STAGGER_MS: u64 = 1500;
// Backoff and jitter belong to F4; this is a fixed retry.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
// A burst of slot changes (a player joining, picking a civ, readying up) should
// cost the game bot one register, not one per change.
const REGISTER_DEBOUNCE: Duration = Duration::from_millis(500);
// Shutdown must not hang on a lobby server that stopped answering.
const STREAM_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

fn default_server_name() -> String {
    "Veredus".to_string()
}

fn default_engine_version() -> String {
    "0.28.0".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct XmppCredentials {
    pub jid: String,
    pub password: String,
}

// Unknown fields are ignored, so an existing 0ad-dedicated lobby.json loads
// unchanged.
#[derive(Debug, Deserialize, Clone)]
pub struct LobbyConfig {
    pub accounts: Vec<XmppCredentials>,
    pub muc_room: String,
    pub bot_jid: String,
    pub public_ip: String,
    #[serde(default = "default_server_name")]
    pub server_name: String,
    #[serde(default = "default_engine_version")]
    pub engine_version: String,
    #[serde(default)]
    pub game_password: String,
}

// Main <- XMPP account task.
pub enum LobbyEvent {
    HostRequested {
        account: usize,
        sender: String,
        host_jid: String,
    },
    GameEnded {
        account: usize,
    },
}

// LobbyManager -> XMPP account task.
enum AccountControl {
    Assign {
        auth_tx: std::sync::mpsc::Sender<LobbyAuthToken>,
        events_rx: mpsc::UnboundedReceiver<GameToLobby>,
        port: u16,
        password_hash: String,
    },
    Release,
    Shutdown,
}

struct AccountSlot {
    control_tx: mpsc::Sender<AccountControl>,
    handle: Option<JoinHandle<()>>,
    in_use: bool,
}

pub struct LobbyManager {
    config: LobbyConfig,
    accounts: Vec<AccountSlot>,
}

impl LobbyManager {
    pub fn new(config: LobbyConfig) -> Self {
        Self {
            config,
            accounts: Vec::new(),
        }
    }

    // Spawns one tokio task per account, staggered by index. Must be called
    // from within a tokio context.
    pub fn start(&mut self) -> mpsc::Receiver<LobbyEvent> {
        let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();

        let bot_jid: Jid =
            self.config.bot_jid.parse().unwrap_or_else(|error| {
                panic!("invalid bot_jid '{}': {error:?}", self.config.bot_jid)
            });

        let account_config = AccountConfig {
            muc_room: self.config.muc_room.clone(),
            bot_jid,
            public_ip: self.config.public_ip.clone(),
            server_name: self.config.server_name.clone(),
            engine_version: self.config.engine_version.clone(),
            has_password: !self.config.game_password.is_empty(),
        };

        let (main_tx, main_rx) = mpsc::channel::<LobbyEvent>(32);

        for (idx, creds) in self.config.accounts.iter().enumerate() {
            let (control_tx, control_rx) = mpsc::channel::<AccountControl>(16);

            let creds = creds.clone();
            let account_config = account_config.clone();
            let task_main_tx = main_tx.clone();

            let span = tracing::info_span!("lobby_account", account = idx, jid = %creds.jid);

            let handle = tokio::spawn(
                async move {
                    if idx > 0 {
                        tokio::time::sleep(Duration::from_millis(
                            idx as u64 * ACCOUNT_SPAWN_STAGGER_MS,
                        ))
                        .await;
                    }
                    run_account(idx, creds, account_config, control_rx, task_main_tx).await;
                }
                .instrument(span),
            );

            self.accounts.push(AccountSlot {
                control_tx,
                handle: Some(handle),
                in_use: false,
            });
        }

        main_rx
    }

    // Reserves the account that received the hostme, marking it busy so a
    // second caller cannot double-book it before `assign` follows. It has to
    // be that account and not any free one: the game password is salted with
    // the receiving account's bound JID.
    pub fn reserve(&mut self, account: usize) -> bool {
        match self.accounts.get_mut(account) {
            Some(slot) if !slot.in_use => {
                slot.in_use = true;
                true
            }
            _ => false,
        }
    }

    pub fn assign(
        &self,
        account: usize,
        auth_tx: std::sync::mpsc::Sender<LobbyAuthToken>,
        events_rx: mpsc::UnboundedReceiver<GameToLobby>,
        port: u16,
        password_hash: String,
    ) -> bool {
        let Some(slot) = self.accounts.get(account) else {
            return false;
        };
        // The queue holds at most one message per game, so a failure here
        // means the account task is gone: the caller must not hand it a game.
        match slot.control_tx.try_send(AccountControl::Assign {
            auth_tx,
            events_rx,
            port,
            password_hash,
        }) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::error!(account, "lobby account control queue full");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!(account, "lobby account task has exited");
                false
            }
        }
    }

    // Also the right call after a failed `reserve` + game-creation attempt:
    // an account that was never assigned just ignores the message.
    pub fn release(&mut self, account: usize) {
        if let Some(slot) = self.accounts.get_mut(account) {
            slot.in_use = false;
            // Best effort: the task already drops its assignment when the
            // game's channel closes, so a lost Release changes nothing.
            let _ = slot.control_tx.try_send(AccountControl::Release);
        }
    }

    pub async fn shutdown(&mut self) {
        for (idx, slot) in self.accounts.iter().enumerate() {
            tracing::info!(account = idx, "sending shutdown to lobby account");
            // Best effort: a task that cannot receive this has already exited,
            // and the join below returns at once for it.
            let _ = slot.control_tx.try_send(AccountControl::Shutdown);
        }
        for (idx, slot) in self.accounts.iter_mut().enumerate() {
            if let Some(handle) = slot.handle.take() {
                tracing::info!(account = idx, "waiting for lobby account to finish");
                let _ = handle.await;
            }
        }
    }
}

#[derive(Clone)]
struct AccountConfig {
    muc_room: String,
    bot_jid: Jid,
    public_ip: String,
    server_name: String,
    engine_version: String,
    has_password: bool,
}

// What this account knows about the game it currently hosts.
struct Assigned {
    auth_tx: std::sync::mpsc::Sender<LobbyAuthToken>,
    events_rx: mpsc::UnboundedReceiver<GameToLobby>,
    assignment: Assignment,
    // Sec. 17.4: scoped to this assignment, not the account's whole lifetime.
    failures: HashMap<String, u32>,
}

// The register IQs one account owes the game bot. A window opens on the first
// change and whatever is newest when it closes is sent, so no clock read is
// needed to debounce.
#[derive(Default)]
struct Registration {
    last_sent: Option<HashMap<String, String>>,
    pending: Option<HashMap<String, String>>,
    debounce: Option<Pin<Box<Sleep>>>,
}

impl Registration {
    // Called whenever the bot's view is gone or belongs to another game, so the
    // next listing is never suppressed as unchanged.
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn offer(&mut self, attrs: HashMap<String, String>) {
        if self.pending.is_none() && self.last_sent.as_ref() == Some(&attrs) {
            return;
        }
        self.pending = Some(attrs);
        if self.debounce.is_none() {
            self.debounce = Some(Box::pin(tokio::time::sleep(REGISTER_DEBOUNCE)));
        }
    }

    async fn flush(&mut self, client: &mut Client, config: &AccountConfig) {
        self.debounce = None;
        let Some(attrs) = self.pending.take() else {
            return;
        };
        if self.last_sent.as_ref() == Some(&attrs) {
            return;
        }
        send_gamelist(client, config, gamelist::register(&attrs)).await;
        self.last_sent = Some(attrs);
    }
}

fn build_muc_presence(muc_room: &str, bound_jid: &Jid) -> Presence {
    let nickname = bound_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "dedicated".to_string());
    let to: Jid = format!("{muc_room}/{nickname}")
        .parse()
        .expect("muc_room plus a JID nickname is a valid JID");

    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(bound_jid.clone());
    presence.to = Some(to);
    presence.add_payload(Muc::new());
    presence
}

// Main loop for one lobby account. Reconnects on stream end with a fixed
// delay (F4 owns backoff and jitter).
async fn run_account(
    account: usize,
    creds: XmppCredentials,
    config: AccountConfig,
    mut control_rx: mpsc::Receiver<AccountControl>,
    main_tx: mpsc::Sender<LobbyEvent>,
) {
    // Generated once per task, not per connection: clients salt the game
    // password with the full JID (PROTOCOL.md Sec. 18), so it must stay
    // stable across reconnects. The lobby bot also filters IQs by resource
    // prefix, mirroring what the stock client does ("0ad-" + a fresh guid).
    let resource = format!("0ad-{}", uuid::Uuid::new_v4());
    let jid: Jid = format!("{}/{resource}", creds.jid)
        .parse()
        .unwrap_or_else(|error| panic!("invalid lobby account jid '{}': {error:?}", creds.jid));
    let username = jid.node().map(|n| n.to_string()).unwrap_or_default();
    // The lobby server stores the SASL-hashed form, so the client-side hash
    // has to happen before login rather than being left to XMPP SASL itself.
    let login_password = sasl_password(&creds.password, &username);

    let mut assigned: Option<Assigned> = None;
    let mut registration = Registration::default();

    loop {
        tracing::info!("connecting to lobby");
        let mut client = Client::new(jid.clone(), login_password.clone());
        let mut bound_jid: Option<Jid> = None;
        // The bot dropped this account's game when the old stream went away.
        registration.clear();

        let shutdown = 'connection: loop {
            tokio::select! {
                control = control_rx.recv() => {
                    match control {
                        Some(AccountControl::Assign { auth_tx, events_rx, port, password_hash }) => {
                            tracing::info!(port, "lobby account assigned a game");
                            assigned = Some(Assigned {
                                auth_tx,
                                events_rx,
                                assignment: Assignment { port, password_hash },
                                failures: HashMap::new(),
                            });
                            registration.clear();
                        }
                        Some(AccountControl::Release) => {
                            tracing::info!("lobby account released its game");
                            assigned = None;
                            registration.clear();
                        }
                        Some(AccountControl::Shutdown) | None => break 'connection true,
                    }
                }

                game_event = async {
                    match &mut assigned {
                        Some(a) => a.events_rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match (game_event, bound_jid.as_ref()) {
                        (Some(GameToLobby::Listing { host_username, nbp, players }), Some(host_jid)) => {
                            let host_jid = host_jid.to_string();
                            registration.offer(gamelist::register_attrs(gamelist::RegisterAttrs {
                                server_name: &config.server_name,
                                engine_version: &config.engine_version,
                                host_username: &host_username,
                                host_jid: &host_jid,
                                nbp,
                                players: &players,
                                has_password: config.has_password,
                            }));
                        }
                        (Some(GameToLobby::Started { nbp, players }), Some(_)) => {
                            // The bot expects the final register before changestate,
                            // so a listing still inside its window goes out now.
                            registration.flush(&mut client, &config).await;
                            send_gamelist(&mut client, &config, gamelist::changestate(nbp, &players)).await;
                        }
                        (Some(_), None) => {}
                        (None, _) => {
                            // The game thread dropped its sender: the match ended.
                            assigned = None;
                            registration.clear();
                            if bound_jid.is_some() {
                                send_unregister(&mut client, &config).await;
                            }
                            let _ = main_tx.send(LobbyEvent::GameEnded { account }).await;
                        }
                    }
                }

                () = async {
                    match &mut registration.debounce {
                        Some(debounce) => debounce.as_mut().await,
                        None => std::future::pending().await,
                    }
                } => {
                    registration.flush(&mut client, &config).await;
                }

                event = client.next() => {
                    let Some(event) = event else {
                        tracing::warn!("lobby XMPP stream ended");
                        break 'connection false;
                    };
                    if event.is_online() {
                        let online_jid = event.get_jid().cloned().unwrap_or_else(|| jid.clone());
                        tracing::info!(bound_jid = %online_jid, "lobby account online");
                        bound_jid = Some(online_jid.clone());
                        let presence = build_muc_presence(&config.muc_room, &online_jid);
                        let _ = client.send_stanza(presence.into()).await;
                    } else if let Some(stanza) = event.into_stanza() {
                        handle_stanza(
                            stanza,
                            &mut client,
                            &config,
                            bound_jid.as_ref(),
                            &mut assigned,
                            account,
                            &main_tx,
                        )
                        .await;
                    }
                }
            }
        };

        if shutdown {
            tracing::info!("lobby account shutting down");
            // Closing the stream would also delist the game, via the bot's MUC
            // offline handling, but only once the server notices; unregister
            // takes it off the list immediately.
            if assigned.is_some() && bound_jid.is_some() {
                send_unregister(&mut client, &config).await;
            }
            if tokio::time::timeout(STREAM_CLOSE_TIMEOUT, client.send_end())
                .await
                .is_err()
            {
                tracing::warn!("timed out closing the lobby stream");
            }
            break;
        }

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn handle_stanza(
    stanza: Stanza,
    client: &mut Client,
    config: &AccountConfig,
    bound_jid: Option<&Jid>,
    assigned: &mut Option<Assigned>,
    account: usize,
    main_tx: &mpsc::Sender<LobbyEvent>,
) {
    match stanza {
        Stanza::Iq(iq) => handle_iq(iq, client, config, assigned).await,
        Stanza::Message(msg) => {
            handle_muc_message(msg, bound_jid, assigned, account, main_tx).await
        }
        Stanza::Presence(_) => {}
    }
}

async fn handle_iq(
    iq: Iq,
    client: &mut Client,
    config: &AccountConfig,
    assigned: &mut Option<Assigned>,
) {
    match iq {
        Iq::Set {
            from: Some(from),
            id,
            ref payload,
            ..
        } if payload.is("auth", auth::NS_LOBBYAUTH) => {
            let auth_tx = assigned.as_ref().map(|a| &a.auth_tx);
            auth::handle(client, from, id, payload, auth_tx).await;
        }
        Iq::Get {
            from: Some(from),
            id,
            ref payload,
            ..
        } if payload.is("connectiondata", connection_data::NS_CONNECTIONDATA) => {
            let assigned_ref = assigned.as_mut().map(|a| (&a.assignment, &mut a.failures));
            connection_data::handle(client, from, id, payload, assigned_ref, &config.public_ip)
                .await;
        }
        Iq::Get {
            from: Some(from),
            id,
            ref payload,
            ..
        } if payload.is("ping", "urn:xmpp:ping") => {
            let result = Iq::empty_result(from, id);
            let _ = client.send_stanza(result.into()).await;
        }
        Iq::Error {
            from, id, error, ..
        } => {
            tracing::debug!(?from, %id, ?error, "IQ error from lobby");
        }
        _ => {}
    }
}

// Only a MUC groupchat "hostme" with no game already assigned turns into a
// `HostRequested` event. Every outcome, including a failure, is logged only:
// MUC chat replies stay suppressed (not allowed).
async fn handle_muc_message(
    msg: XmppMessage,
    bound_jid: Option<&Jid>,
    assigned: &Option<Assigned>,
    account: usize,
    main_tx: &mpsc::Sender<LobbyEvent>,
) {
    if msg.type_ != MessageType::Groupchat {
        return;
    }
    // A stanza tagged with delayed delivery is MUC history replayed after a
    // reconnect, not a live command.
    if msg.payloads.iter().any(|p| p.is("delay", "urn:xmpp:delay")) {
        return;
    }
    let body = msg
        .bodies
        .get("")
        .map(|b| b.trim())
        .unwrap_or_default()
        .to_string();
    if body != "hostme" {
        return;
    }
    if assigned.is_some() {
        tracing::debug!("ignoring hostme: this account already hosts a game");
        return;
    }
    let Some(host_jid) = bound_jid else {
        return;
    };
    let sender = msg
        .from
        .as_ref()
        .and_then(|jid| jid.resource())
        .map(|r| r.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    tracing::info!(%sender, "received hostme");
    let _ = main_tx
        .send(LobbyEvent::HostRequested {
            account,
            sender,
            host_jid: host_jid.to_string(),
        })
        .await;
}

async fn send_gamelist(client: &mut Client, config: &AccountConfig, query: Element) {
    let _token = client
        .send_iq(
            Some(config.bot_jid.clone()),
            tokio_xmpp::IqRequest::Set(query),
        )
        .await;
}

async fn send_unregister(client: &mut Client, config: &AccountConfig) {
    send_gamelist(client, config, gamelist::unregister()).await;
}

// Ported from 0 A.D.'s EncryptPassword() (JSInterface_Lobby.cpp). The client
// hashes the plaintext password with this before XMPP SASL authentication, so
// the lobby server stores the hashed form and the relay must log in with the
// same transformation:
//   salt = SHA-256(SALT_BASE || username)
//   output = PBKDF2-HMAC*-SHA256(password, salt, 1337 rounds)
//            (* non-standard HMAC: a 32-byte padding block, not SHA-256's 64)
//   result = uppercase hex of output
fn sasl_password(password: &str, username: &str) -> String {
    const DIGEST_SIZE: usize = 32;
    const ITERATIONS: u32 = 1337;
    const SALT_BASE: [u8; DIGEST_SIZE] = [
        244, 243, 249, 244, 32, 33, 34, 35, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 32, 33,
        244, 224, 127, 129, 130, 140, 153, 133, 123, 234, 123,
    ];

    let mut hasher = Sha256::new();
    hasher.update(SALT_BASE);
    hasher.update(username.as_bytes());
    let salt: [u8; DIGEST_SIZE] = hasher.finalize().into();

    let key = password.as_bytes();
    let mut asalt = [0u8; DIGEST_SIZE + 4];
    asalt[..DIGEST_SIZE].copy_from_slice(&salt);
    // Block count 1, big-endian; PBKDF2 needs only one block for a 32-byte key.
    asalt[DIGEST_SIZE + 3] = 1;

    let mut block = hmac_sha256_nonstandard(&asalt, key);
    let mut out = block;
    for _ in 1..ITERATIONS {
        block = hmac_sha256_nonstandard(&block, key);
        for (o, b) in out.iter_mut().zip(block.iter()) {
            *o ^= b;
        }
    }

    hex::encode_upper(out)
}

fn hmac_sha256_nonstandard(text: &[u8], key: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 32;

    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        key_block.copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = key_block;
    for b in &mut inner_pad {
        *b ^= 0x36;
    }
    let mut hasher = Sha256::new();
    hasher.update(inner_pad);
    hasher.update(text);
    let inner_hash = hasher.finalize();

    let mut outer_pad = key_block;
    for b in &mut outer_pad {
        *b ^= 0x5c;
    }
    let mut hasher = Sha256::new();
    hasher.update(outer_pad);
    hasher.update(inner_hash);
    hasher.finalize().into()
}
