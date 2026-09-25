// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

pub mod auth;
pub mod connection_data;
pub mod game_report;
pub mod gamelist;
pub mod link;
pub mod rating;
pub mod terms;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::stream::StreamExt;
use sasl::common::ChannelBinding;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tokio_xmpp::Client;
use tokio_xmpp::Event;
use tokio_xmpp::Stanza;
use tokio_xmpp::connect::DnsConfig;
use tokio_xmpp::connect::ServerConnector;
use tokio_xmpp::connect::StartTlsServerConnector;
use tokio_xmpp::minidom::Element;
use tokio_xmpp::parsers::iq::Iq;
use tokio_xmpp::parsers::jid::BareJid;
use tokio_xmpp::parsers::jid::Jid;
use tokio_xmpp::parsers::message::Message as XmppMessage;
use tokio_xmpp::parsers::message::MessageType;
use tokio_xmpp::parsers::muc::Muc;
use tokio_xmpp::parsers::presence::Presence;
use tokio_xmpp::parsers::presence::Show;
use tokio_xmpp::parsers::presence::Type as PresenceType;
use tokio_xmpp::xmlstream::PendingFeaturesRecv;
use tokio_xmpp::xmlstream::Timeouts;
use tracing::Instrument;

use crate::lobby::connection_data::Assignment;
use crate::lobby::link::GameReport;
use crate::lobby::link::GameToLobby;
use crate::lobby::link::LobbyToGame;

// Spread out so the lobby server's TCP accept queue never sees every account
// connect in the same instant.
const ACCOUNT_SPAWN_STAGGER_MS: u64 = 1500;
// A dropped connection is retried by tokio-xmpp's own reconnector, with its
// own backoff. This delay only paces restarting a client whose stream ended
// for good, which is rare enough that a fixed value does.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
// tokio-xmpp retries a dropped connection at once and then backs off in
// lockstep, so a lobby server restart would see every account at the same
// instants. Each attempt is delayed by a random share of the startup stagger's
// window instead, capped at tokio-xmpp's own longest backoff step.
const RECONNECT_JITTER_MAX: Duration = Duration::from_secs(30);
// A burst of slot changes (a player joining, picking a civ, readying up) should
// cost the game bot one register, not one per change.
const REGISTER_DEBOUNCE: Duration = Duration::from_millis(500);
// Shutdown must not hang on a lobby server that stopped answering.
const STREAM_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn default_server_name() -> String {
    "Veredus".to_string()
}

pub(crate) fn default_engine_version() -> String {
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
    // Some selects personal mode, which is configured only from the TOML
    // file's [personal] table.
    #[serde(skip)]
    pub personal: Option<PersonalLobby>,
}

// The player whose own account hosts in personal mode.
#[derive(Debug, Clone)]
pub struct PersonalLobby {
    // As typed into the stock login: its MUC nick and the salt of its
    // password hash both keep that capitalization.
    pub name: String,
    pub rating_bot_jid: String,
}

impl LobbyConfig {
    // The account tasks parse these only once logging is up, where a typo
    // could only panic; checking here turns it into a startup error.
    pub fn validate(&self) -> Result<(), String> {
        self.bot_jid
            .parse::<Jid>()
            .map_err(|error| format!("invalid lobby bot_jid '{}': {error}", self.bot_jid))?;
        self.muc_room
            .parse::<BareJid>()
            .map_err(|error| format!("invalid lobby muc_room '{}': {error}", self.muc_room))?;
        if let Some(personal) = &self.personal {
            personal.rating_bot_jid.parse::<Jid>().map_err(|error| {
                format!(
                    "invalid rating_bot_jid '{}': {error}",
                    personal.rating_bot_jid
                )
            })?;
        }
        // Parsed with a resource of the shape the account task appends, so
        // this accepts exactly what the task will.
        let resource = format!("0ad-{}", uuid::Uuid::nil());
        for creds in &self.accounts {
            format!("{}/{resource}", creds.jid)
                .parse::<Jid>()
                .map_err(|error| format!("invalid lobby account jid '{}': {error}", creds.jid))?;
        }
        Ok(())
    }
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
        auth_tx: std::sync::mpsc::Sender<LobbyToGame>,
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
    // The full JID the account binds, which the game password is salted
    // with. Known before the account is online, so a resumed match can be
    // hosted without waiting for a hostme to report it.
    host_jid: String,
    report_tx: mpsc::UnboundedSender<GameReport>,
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
    pub fn start(&mut self) -> mpsc::UnboundedReceiver<LobbyEvent> {
        let _ = tokio_xmpp::rustls::crypto::aws_lc_rs::default_provider().install_default();

        let bot_jid: Jid = self
            .config
            .bot_jid
            .parse()
            .expect("bot_jid is checked by LobbyConfig::validate");

        let muc_bare: BareJid = self
            .config
            .muc_room
            .parse()
            .expect("muc_room is checked by LobbyConfig::validate");

        let personal = self.config.personal.as_ref().map(|p| PersonalAccount {
            nick: p.name.clone(),
            rating_bot_jid: p
                .rating_bot_jid
                .parse()
                .expect("rating_bot_jid is checked by LobbyConfig::validate"),
        });

        let account_config = AccountConfig {
            muc_room: self.config.muc_room.clone(),
            muc_bare,
            bot_jid,
            personal,
            public_ip: self.config.public_ip.clone(),
            server_name: self.config.server_name.clone(),
            has_password: !self.config.game_password.is_empty(),
            reconnect_spread: Duration::from_millis(
                self.config.accounts.len() as u64 * ACCOUNT_SPAWN_STAGGER_MS,
            )
            .min(RECONNECT_JITTER_MAX),
        };

        // Unbounded so an account task never blocks on a busy main loop: it
        // would stop answering pings and the IQs of the game it hosts.
        let (main_tx, main_rx) = mpsc::unbounded_channel::<LobbyEvent>();

        for (idx, creds) in self.config.accounts.iter().enumerate() {
            let (control_tx, control_rx) = mpsc::channel::<AccountControl>(16);
            let (report_tx, report_rx) = mpsc::unbounded_channel::<GameReport>();

            let creds = creds.clone();
            let account_config = account_config.clone();
            let task_main_tx = main_tx.clone();
            // Generated once per task, not per connection: clients salt the
            // game password with the full JID, so it must stay stable across
            // reconnects. The lobby bot also filters
            // IQs by resource prefix, mirroring what the stock client does
            // ("0ad-" + a fresh guid).
            let resource = format!("0ad-{}", uuid::Uuid::new_v4());
            let host_jid = format!("{}/{resource}", creds.jid);

            let span = tracing::info_span!("lobby_account", account = idx, jid = %creds.jid);

            let handle = tokio::spawn(
                async move {
                    if idx > 0 {
                        tokio::time::sleep(Duration::from_millis(
                            idx as u64 * ACCOUNT_SPAWN_STAGGER_MS,
                        ))
                        .await;
                    }
                    run_account(
                        idx,
                        creds,
                        resource,
                        account_config,
                        control_rx,
                        report_rx,
                        task_main_tx,
                    )
                    .await;
                }
                .instrument(span),
            );

            self.accounts.push(AccountSlot {
                control_tx,
                handle: Some(handle),
                in_use: false,
                host_jid,
                report_tx,
            });
        }

        crate::metrics::LOBBY_ACCOUNTS.set(self.accounts.len() as i64);
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
                crate::metrics::LOBBY_ACCOUNTS_BUSY.inc();
                true
            }
            _ => false,
        }
    }

    // For a resumed match, which has no hostme to tie it to one account:
    // the account that hosted it before when that one is free, else any.
    pub fn reserve_free(&mut self, preferred: Option<usize>) -> Option<usize> {
        let account = preferred
            .filter(|&i| self.accounts.get(i).is_some_and(|slot| !slot.in_use))
            .or_else(|| self.accounts.iter().position(|slot| !slot.in_use))?;
        self.reserve(account).then_some(account)
    }

    pub fn host_jid(&self, account: usize) -> Option<&str> {
        self.accounts
            .get(account)
            .map(|slot| slot.host_jid.as_str())
    }

    // Personal mode only: where a finished match's rated report goes. It
    // outlives the game, whose own channel is gone before the report is.
    pub fn report_sender(&self, account: usize) -> Option<mpsc::UnboundedSender<GameReport>> {
        self.config.personal.as_ref()?;
        self.accounts
            .get(account)
            .map(|slot| slot.report_tx.clone())
    }

    pub fn assign(
        &self,
        account: usize,
        auth_tx: std::sync::mpsc::Sender<LobbyToGame>,
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
            // Release is also sent to accounts that were never reserved.
            if std::mem::replace(&mut slot.in_use, false) {
                crate::metrics::LOBBY_ACCOUNTS_BUSY.dec();
            }
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
    muc_bare: BareJid,
    bot_jid: Jid,
    // Some in personal mode, where the account is one player's own.
    personal: Option<PersonalAccount>,
    public_ip: String,
    server_name: String,
    has_password: bool,
    reconnect_spread: Duration,
}

#[derive(Clone)]
struct PersonalAccount {
    nick: String,
    rating_bot_jid: Jid,
}

// Wraps the connector `Client::new` would use. tokio-xmpp calls `connect`
// once for the first connection and again for every reconnect attempt, and
// never tells the caller the stream dropped, so this is the one place that
// can both spread reconnects out and log that they happen.
#[derive(Debug, Clone)]
struct JitteredConnector {
    inner: StartTlsServerConnector,
    spread: Duration,
    // Shared by the clones tokio-xmpp makes per attempt.
    reconnecting: Arc<AtomicBool>,
}

impl JitteredConnector {
    fn new(jid: &Jid, spread: Duration) -> Self {
        Self {
            inner: StartTlsServerConnector::from(DnsConfig::srv_default_client(
                jid.domain().as_ref(),
            )),
            spread,
            reconnecting: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ServerConnector for JitteredConnector {
    type Stream = <StartTlsServerConnector as ServerConnector>::Stream;

    async fn connect(
        &self,
        jid: &Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), tokio_xmpp::Error> {
        // The first connection was already spaced out by the startup stagger.
        if self.reconnecting.swap(true, Ordering::Relaxed) {
            let delay = reconnect_jitter(self.spread);
            tracing::info!(delay_ms = delay.as_millis() as u64, "reconnecting to lobby");
            tokio::time::sleep(delay).await;
        }
        self.inner.connect(jid, ns, timeouts).await
    }
}

// Uniform in [0, spread). A v4 uuid is the only randomness source already
// among the dependencies.
fn reconnect_jitter(spread: Duration) -> Duration {
    let spread_ms = spread.as_millis();
    if spread_ms == 0 {
        return Duration::ZERO;
    }
    let ms = uuid::Uuid::new_v4().as_u128() % spread_ms;
    Duration::from_millis(ms as u64)
}

// What this account knows about the game it currently hosts.
struct Assigned {
    auth_tx: std::sync::mpsc::Sender<LobbyToGame>,
    events_rx: mpsc::UnboundedReceiver<GameToLobby>,
    assignment: Assignment,
    // Sec. 17.4: scoped to this assignment, not the account's whole lifetime.
    failures: HashMap<String, u32>,
    // Set once the listing was withdrawn at the end of the match, so the
    // game thread exiting does not withdraw it a second time.
    unlisted: bool,
}

// The register IQs one account owes the game bot. A window opens on the first
// change and whatever is newest when it closes is sent, so no clock read is
// needed to debounce.
#[derive(Default)]
struct Registration {
    last_sent: Option<HashMap<String, String>>,
    pending: Option<HashMap<String, String>>,
    debounce: Option<Pin<Box<Sleep>>>,
    // The last changestate sent, kept because a match in progress sends no
    // further listings that could restore it after a reconnect.
    started: Option<(u32, String)>,
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

    // The bot deletes a host's game when its session leaves the MUC, so a new
    // session has to list it again, newest attributes first. Changestate goes
    // after the register because a register resets the bot's state to "init".
    async fn resend(&mut self, client: &mut Client, config: &AccountConfig) {
        self.debounce = None;
        let Some(attrs) = self.pending.take().or_else(|| self.last_sent.take()) else {
            return;
        };
        tracing::info!("re-registering the game after a new lobby session");
        send_gamelist(client, config, gamelist::register(&attrs)).await;
        self.last_sent = Some(attrs);
        if let Some((nbp, players)) = &self.started {
            send_gamelist(client, config, gamelist::changestate(*nbp, players)).await;
        }
    }
}

fn muc_nickname(config: &AccountConfig, bound_jid: &Jid) -> String {
    match &config.personal {
        Some(personal) => personal.nick.clone(),
        None => bound_jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "dedicated".to_string()),
    }
}

fn muc_occupant(config: &AccountConfig, bound_jid: &Jid) -> Jid {
    let nickname = muc_nickname(config, bound_jid);
    format!("{}/{nickname}", config.muc_room)
        .parse()
        .expect("muc_room plus a JID nickname is a valid JID")
}

fn build_muc_presence(config: &AccountConfig, bound_jid: &Jid) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(bound_jid.clone());
    presence.to = Some(muc_occupant(config, bound_jid));
    presence.add_payload(Muc::new());
    presence
}

// Personal mode shows the account the way a stock client shows its player:
// "playing" (dnd) while it hosts a listed game, available otherwise. The
// client sends these to its room occupant without the join payload.
async fn send_playing(client: &mut Client, config: &AccountConfig, bound_jid: &Jid, playing: bool) {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(bound_jid.clone());
    presence.to = Some(muc_occupant(config, bound_jid));
    presence.show = playing.then_some(Show::Dnd);
    let _ = client.send_stanza(presence.into()).await;
}

// Only a change is sent, and only once online: a new session sends the
// current state itself after joining the room.
async fn set_playing(
    client: &mut Client,
    config: &AccountConfig,
    bound_jid: Option<&Jid>,
    playing: &mut bool,
    value: bool,
) {
    if config.personal.is_none() || *playing == value {
        return;
    }
    *playing = value;
    if let Some(bound_jid) = bound_jid {
        send_playing(client, config, bound_jid, value).await;
    }
}

async fn send_report(client: &mut Client, config: &AccountConfig, report: &GameReport) {
    let Some(personal) = &config.personal else {
        return;
    };
    tracing::info!("sending the rated game report");
    let _token = client
        .send_iq(
            Some(personal.rating_bot_jid.clone()),
            tokio_xmpp::IqRequest::Set(rating::report(report)),
        )
        .await;
}

// Main loop for one lobby account. A dropped connection never surfaces here:
// tokio-xmpp reconnects on its own and reports only the new session, as
// another Online event. The outer loop only replaces a client whose stream
// ended for good.
async fn run_account(
    account: usize,
    creds: XmppCredentials,
    resource: String,
    config: AccountConfig,
    mut control_rx: mpsc::Receiver<AccountControl>,
    mut report_rx: mpsc::UnboundedReceiver<GameReport>,
    main_tx: mpsc::UnboundedSender<LobbyEvent>,
) {
    let jid: Jid = format!("{}/{resource}", creds.jid)
        .parse()
        .expect("account jids are checked by LobbyConfig::validate");
    // The stock client salts with the name as typed, which the JID may not
    // keep.
    let username = match &config.personal {
        Some(personal) => personal.nick.clone(),
        None => jid.node().map(|n| n.to_string()).unwrap_or_default(),
    };
    // The lobby server stores the SASL-hashed form, so the client-side hash
    // has to happen before login rather than being left to XMPP SASL itself.
    let login_password = sasl_password(&creds.password, &username);

    let mut assigned: Option<Assigned> = None;
    let mut registration = Registration::default();
    // Personal mode only.
    let mut playing = false;
    let mut boardlist_sent = false;
    let mut unsent_reports: Vec<GameReport> = Vec::new();

    loop {
        tracing::info!("connecting to lobby");
        let mut client = Client::new_with_connector(
            jid.clone(),
            login_password.clone(),
            JitteredConnector::new(&jid, config.reconnect_spread),
            Timeouts::default(),
        );
        let mut bound_jid: Option<Jid> = None;

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
                                unlisted: false,
                            });
                            registration.clear();
                        }
                        Some(AccountControl::Release) => {
                            tracing::info!("lobby account released its game");
                            assigned = None;
                            registration.clear();
                            set_playing(&mut client, &config, bound_jid.as_ref(), &mut playing, false).await;
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
                    // State is kept even while a replacement client is not
                    // online yet, and only the sending waits: its first
                    // session is never resumed, so `resend` lists the game
                    // from this state once it is online.
                    match game_event {
                        Some(GameToLobby::Listing { host_username, nbp, players, map, mods }) => {
                            // The resource is fixed for the task, so the
                            // requested JID is what the session will be bound to.
                            let host_jid = bound_jid.as_ref().unwrap_or(&jid).to_string();
                            registration.offer(gamelist::register_attrs(gamelist::RegisterAttrs {
                                server_name: &config.server_name,
                                mods: &mods,
                                host_username: &host_username,
                                host_jid: &host_jid,
                                nbp,
                                players: &players,
                                has_password: config.has_password,
                                map: map.as_ref(),
                            }));
                            if let Some(a) = assigned.as_mut() {
                                a.unlisted = false;
                            }
                            set_playing(&mut client, &config, bound_jid.as_ref(), &mut playing, true).await;
                        }
                        Some(GameToLobby::Started { nbp, players }) => {
                            if bound_jid.is_some() {
                                // The bot expects the final register before changestate,
                                // so a listing still inside its window goes out now.
                                registration.flush(&mut client, &config).await;
                                send_gamelist(&mut client, &config, gamelist::changestate(nbp, &players)).await;
                            }
                            registration.started = Some((nbp, players));
                        }
                        Some(GameToLobby::Ended) => {
                            registration.clear();
                            if bound_jid.is_some() {
                                send_unregister(&mut client, &config).await;
                            }
                            if let Some(a) = assigned.as_mut() {
                                a.unlisted = true;
                            }
                        }
                        Some(GameToLobby::Unlisted) => {
                            let already = assigned.as_ref().is_some_and(|a| a.unlisted);
                            registration.clear();
                            if bound_jid.is_some() && !already {
                                tracing::info!("withdrawing the listing until the host is back");
                                send_unregister(&mut client, &config).await;
                            }
                            if let Some(a) = assigned.as_mut() {
                                a.unlisted = true;
                            }
                            set_playing(&mut client, &config, bound_jid.as_ref(), &mut playing, false).await;
                        }
                        None => {
                            // The game thread dropped its sender: the match ended.
                            let unlisted = assigned.as_ref().is_some_and(|a| a.unlisted);
                            assigned = None;
                            registration.clear();
                            if bound_jid.is_some() && !unlisted {
                                send_unregister(&mut client, &config).await;
                            }
                            set_playing(&mut client, &config, bound_jid.as_ref(), &mut playing, false).await;
                            let _ = main_tx.send(LobbyEvent::GameEnded { account });
                        }
                    }
                }

                Some(report) = report_rx.recv() => {
                    if bound_jid.is_some() {
                        send_report(&mut client, &config, &report).await;
                    } else {
                        unsent_reports.push(report);
                    }
                }

                () = async {
                    match &mut registration.debounce {
                        Some(debounce) => debounce.as_mut().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if bound_jid.is_some() {
                        registration.flush(&mut client, &config).await;
                    } else {
                        // `pending` stays for `resend` once the client is online.
                        registration.debounce = None;
                    }
                }

                event = client.next() => {
                    let Some(event) = event else {
                        tracing::warn!("lobby XMPP stream ended");
                        crate::metrics::LOBBY_STREAM_ENDED_TOTAL.inc();
                        break 'connection false;
                    };
                    if let Event::Online {
                        bound_jid: online_jid,
                        resumed,
                        ..
                    } = event {
                        tracing::info!(bound_jid = %online_jid, resumed, "lobby account online");
                        crate::metrics::LOBBY_SESSIONS_TOTAL
                            .with_label_values(&[if resumed { "true" } else { "false" }])
                            .inc();
                        bound_jid = Some(online_jid.clone());
                        let presence = build_muc_presence(&config, &online_jid);
                        let _ = client.send_stanza(presence.into()).await;
                        if config.personal.is_some() {
                            if playing {
                                send_playing(&mut client, &config, &online_jid, true).await;
                            }
                            // Once per login, as the stock client does from its
                            // login page; its own reconnects do not repeat it.
                            if !boardlist_sent
                                && let Some(personal) = &config.personal
                            {
                                boardlist_sent = true;
                                let _token = client
                                    .send_iq(
                                        Some(personal.rating_bot_jid.clone()),
                                        tokio_xmpp::IqRequest::Get(rating::get_leaderboard()),
                                    )
                                    .await;
                            }
                            for report in std::mem::take(&mut unsent_reports) {
                                send_report(&mut client, &config, &report).await;
                            }
                        }
                        // A resumed session never left the MUC, so the bot
                        // still has the game.
                        if !resumed {
                            registration.resend(&mut client, &config).await;
                        }
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
    main_tx: &mpsc::UnboundedSender<LobbyEvent>,
) {
    match stanza {
        Stanza::Iq(iq) => handle_iq(iq, client, config, assigned).await,
        Stanza::Message(msg) => {
            handle_muc_message(msg, config, bound_jid, assigned, account, main_tx)
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
            let handed_out =
                connection_data::handle(client, from, id, payload, assigned_ref, &config.public_ip)
                    .await;
            if handed_out && let Some(a) = assigned.as_ref() {
                let _ = a.auth_tx.send(LobbyToGame::JoinerExpected);
            }
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
fn handle_muc_message(
    msg: XmppMessage,
    config: &AccountConfig,
    bound_jid: Option<&Jid>,
    assigned: &Option<Assigned>,
    account: usize,
    main_tx: &mpsc::UnboundedSender<LobbyEvent>,
) {
    // A personal account hosts only for its own player, never on request.
    if config.personal.is_some() || msg.type_ != MessageType::Groupchat {
        return;
    }
    // A groupchat-typed message sent straight to our JID would otherwise let
    // its sender pick any resource as the host name, a new one per message.
    if msg.from.as_ref().map(|jid| jid.to_bare()).as_ref() != Some(&config.muc_bare) {
        tracing::debug!(from = ?msg.from, "ignoring groupchat not from the MUC room");
        return;
    }
    // A stanza tagged with delayed delivery is MUC history replayed after a
    // reconnect, not a live command.
    if msg.payloads.iter().any(|p| p.is("delay", "urn:xmpp:delay")) {
        return;
    }
    // The stock client tags its body with xml:lang, so it is not stored
    // under the empty language.
    let body = msg
        .get_best_body(Vec::new())
        .map(|(_, b)| b.trim())
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
    let _ = main_tx.send(LobbyEvent::HostRequested {
        account,
        sender,
        host_jid: host_jid.to_string(),
    });
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
