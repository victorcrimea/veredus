// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// The black-box driver the whole suite is built on. See TEST_PLAN.md section
// 4 for the constraints this has to respect: nondeterministic broadcast
// order, the disconnect/removal split, and time as an explicit argument.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write;
use std::net::Ipv4Addr;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use flate2::Compression;
use flate2::write::ZlibEncoder;

use veredus::enet::PeerID;
use veredus::relay::gamestate_transfer::KIND_RUNNING_GAME;
use veredus::relay::messages::Authenticate;
use veredus::relay::messages::AuthenticateResult;
use veredus::relay::messages::AuthenticateResultCode;
use veredus::relay::messages::GamestateChunk;
use veredus::relay::messages::GamestateResponse;
use veredus::relay::messages::Guid;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::MapPlayerIdToSlot;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::Syn;
use veredus::relay::messages::SynAck;
use veredus::relay::messages::TurnSealed;
use veredus::relay::messages::WireMessage;
use veredus::relay::monitor::PeerStats;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Idle;
use veredus::relay::server_fsm::Input;
use veredus::relay::server_fsm::Server;

// Effect has no Clone (only WireMessage and DisconnectReason do), because
// nothing in src/ needs to keep an effect around after handing it to the
// transport. The harness wants a running log, so it rebuilds each effect by
// hand rather than asking for one in production code that would serve no
// caller but this one.
fn clone_effect(effect: &Effect) -> Effect {
    match effect {
        Effect::Send { peer, msg } => Effect::Send {
            peer: *peer,
            msg: msg.clone(),
        },
        Effect::Disconnect { peer, reason } => Effect::Disconnect {
            peer: *peer,
            reason: *reason,
        },
        Effect::DisconnectNow { peer, reason } => Effect::DisconnectNow {
            peer: *peer,
            reason: *reason,
        },
        Effect::LobbyListing {
            host_username,
            nbp,
            players,
            map,
            mods,
        } => Effect::LobbyListing {
            host_username: host_username.clone(),
            nbp: *nbp,
            players: players.clone(),
            map: map.clone(),
            mods: mods.clone(),
        },
        Effect::LobbyStarted { nbp, players } => Effect::LobbyStarted {
            nbp: *nbp,
            players: players.clone(),
        },
        Effect::GameOver => Effect::GameOver,
        Effect::StateDump { id, turn, request } => Effect::StateDump {
            id: *id,
            turn: *turn,
            request: request.clone(),
        },
        Effect::CancelStateDump { id } => Effect::CancelStateDump { id: *id },
        Effect::SpawnAiHost { name } => Effect::SpawnAiHost { name: name.clone() },
        Effect::StopAiHost => Effect::StopAiHost,
        Effect::Checkpoint { id, turn, request } => Effect::Checkpoint {
            id: *id,
            turn: *turn,
            request: request.clone(),
        },
        Effect::MatchEnded { checkpoint } => Effect::MatchEnded {
            checkpoint: *checkpoint,
        },
        Effect::PasswordRejected { peer } => Effect::PasswordRejected { peer: *peer },
        Effect::SaveStarted {
            settings,
            ai_settings,
            ai_players,
            lobby_map,
        } => Effect::SaveStarted {
            settings: settings.clone(),
            ai_settings: ai_settings.clone(),
            ai_players: ai_players.clone(),
            lobby_map: lobby_map.clone(),
        },
        Effect::SaveTurn {
            turn,
            length,
            commands,
        } => Effect::SaveTurn {
            turn: *turn,
            length: *length,
            commands: commands.clone(),
        },
        Effect::SaveHash { turn, hash } => Effect::SaveHash {
            turn: *turn,
            hash: hash.clone(),
        },
        Effect::SaveSlots(slots) => Effect::SaveSlots(slots.clone()),
        Effect::SaveStatus(status) => Effect::SaveStatus(*status),
        Effect::SaveState { turn, state } => Effect::SaveState {
            turn: *turn,
            state: state.clone(),
        },
        Effect::SaveAiState { turn, state } => Effect::SaveAiState {
            turn: *turn,
            state: state.clone(),
        },
    }
}

pub const READY: u8 = 1;

pub struct Harness {
    server: Option<AnyServer>,
    pub now: DateTime<Utc>,
    // Every effect ever produced, in emission order. sent_to filters this;
    // within one peer that order is real (4.1), so it is worth keeping whole.
    log: Vec<Effect>,
    // Recorded by connect(), so syn_ack() can echo the version and mod list
    // the server itself just sent rather than a test guessing wire constants.
    syns: HashMap<PeerID, Syn>,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    pub fn with_config(config: Config) -> Self {
        let server: AnyServer = Server::<Idle>::new(config).listen().into();
        Harness {
            server: Some(server),
            // A fixed epoch keeps failure messages readable (4.3).
            now: DateTime::UNIX_EPOCH,
            log: Vec::new(),
            syns: HashMap::new(),
        }
    }

    // For a server built some other way than listening, such as a resumed
    // match. Whatever it pushed while being built is logged as the first
    // effects.
    pub fn with_server(server: AnyServer) -> (Self, Vec<Effect>) {
        let mut h = Harness {
            server: Some(server),
            now: DateTime::UNIX_EPOCH,
            log: Vec::new(),
            syns: HashMap::new(),
        };
        let effects = h
            .server
            .as_mut()
            .expect("harness server missing")
            .take_effects();
        h.log.extend(effects.iter().map(clone_effect));
        (h, effects)
    }

    pub fn server(&self) -> &AnyServer {
        self.server.as_ref().expect("harness server missing")
    }

    // The one place `handle` is called, so the log and the take_effects
    // contract cannot drift apart.
    pub fn input(&mut self, input: Input) -> Vec<Effect> {
        let server = self.server.take().expect("harness server missing");
        let mut server = server.handle(input);
        let effects = server.take_effects();
        self.server = Some(server);
        self.log.extend(effects.iter().map(clone_effect));
        effects
    }

    pub fn tick_at(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        self.tick_with_stats(now, Vec::new())
    }

    // Peer timing only ever reaches the FSM on a tick, so warnings and the
    // AFK hold are driven through here.
    pub fn tick_with_stats(&mut self, now: DateTime<Utc>, stats: Vec<PeerStats>) -> Vec<Effect> {
        self.now = now;
        self.input(Input::Tick { now, stats })
    }

    // No test may call Utc::now() or sleep (4.3); this is how time moves.
    pub fn advance(&mut self, delta: TimeDelta) -> Vec<Effect> {
        self.tick_at(self.now + delta)
    }

    pub fn addr_for(peer: PeerID) -> Ipv4Addr {
        let id = u32::try_from(peer.0).unwrap_or(u32::MAX);
        Ipv4Addr::from(0x0A00_0000u32.wrapping_add(id))
    }

    pub fn connect(&mut self, peer: PeerID) -> Syn {
        let effects = self.input(Input::Connected {
            peer,
            addr: Self::addr_for(peer),
        });
        let syn = expect_send(&effects, peer, |m| match m {
            WireMessage::Syn(s) => Some(s.clone()),
            _ => None,
        })
        .expect("expected a Syn sent to the newly connected peer");
        self.syns.insert(peer, syn.clone());
        syn
    }

    pub fn syn_ack(&mut self, peer: PeerID) -> Guid {
        let syn = self
            .syns
            .get(&peer)
            .expect("connect() was not called for this peer")
            .clone();
        let msg = WireMessage::SynAck(SynAck {
            magic_response: syn.magic,
            protocol_version: syn.protocol_version,
            engine_version: syn.engine_version,
            enabled_mods: syn.enabled_mods,
        });
        let effects = self.input(Input::Received { peer, msg });
        expect_send(&effects, peer, |m| match m {
            WireMessage::Ack(a) => Some(a.clone()),
            _ => None,
        })
        .expect("expected an Ack sent to the newly synced peer")
        .guid
    }

    pub fn authenticate(&mut self, peer: PeerID, name: &str) -> AuthenticateResult {
        self.authenticate_with_password(peer, name, "")
    }

    pub fn authenticate_with_password(
        &mut self,
        peer: PeerID,
        name: &str,
        password: &str,
    ) -> AuthenticateResult {
        let effects = self.send_authenticate(peer, name, password);
        expect_send(&effects, peer, |m| match m {
            WireMessage::AuthenticateResult(r) => Some(r.clone()),
            _ => None,
        })
        .expect("expected an AuthenticateResult sent to the authenticating peer")
    }

    // A refusal disconnects without ever sending an AuthenticateResult, so a
    // test asserting a refusal drives this directly instead of authenticate,
    // which requires the result to be there.
    pub fn send_authenticate(&mut self, peer: PeerID, name: &str, password: &str) -> Vec<Effect> {
        let msg = WireMessage::Authenticate(Authenticate {
            name: name.to_string(),
            password: password.to_string(),
            controller_secret: String::new(),
        });
        self.input(Input::Received { peer, msg })
    }

    // Drives the whole handshake (4.4). Almost every test builds on an
    // admitted session, so a refusal here has to fail loudly rather than
    // send the test chasing a much later, unrelated-looking failure.
    pub fn admit(&mut self, peer: PeerID, name: &str) -> Guid {
        let guid = self.connect_and_ack(peer);
        let result = self.authenticate(peer, name);
        assert!(
            matches!(
                result.code,
                AuthenticateResultCode::Ok | AuthenticateResultCode::OkRejoining
            ),
            "authenticate({name}) was refused with code {:?}",
            result.code
        );
        guid
    }

    fn connect_and_ack(&mut self, peer: PeerID) -> Guid {
        self.connect(peer);
        self.syn_ack(peer)
    }

    // Admits and assigns a slot from `controller`, because the
    // player-versus-observer distinction drives pause, command validation
    // and turn release.
    pub fn admit_as_player(
        &mut self,
        peer: PeerID,
        name: &str,
        slot: i8,
        controller: PeerID,
    ) -> Guid {
        let guid = self.admit(peer, name);
        self.map_player_id_to_slot(controller, slot, &guid);
        guid
    }

    pub fn map_player_id_to_slot(
        &mut self,
        controller: PeerID,
        slot: i8,
        guid: &Guid,
    ) -> Vec<Effect> {
        let msg = WireMessage::MapPlayerIdToSlot(MapPlayerIdToSlot {
            player_id: slot,
            guid: guid.clone(),
        });
        self.input(Input::Received {
            peer: controller,
            msg,
        })
    }

    // The window described in 4.2: a kicked or disconnected peer stays a
    // session, blocking release and counting toward capacity, until this is
    // fed. Named after what actually happens on the wire (ENet reporting the
    // peer gone), not what the FSM call is named.
    pub fn enet_confirms_disconnect(&mut self, peer: PeerID) -> Vec<Effect> {
        self.input(Input::Disconnected { peer })
    }

    // Consumes the harness, because AnyServer::shutdown consumes the server.
    pub fn shutdown(mut self) -> Vec<Effect> {
        self.server
            .take()
            .expect("harness server missing")
            .shutdown("test")
    }

    // Consumes the harness, because AnyServer::stop consumes the server.
    pub fn stop(mut self) -> Vec<Effect> {
        self.server.take().expect("harness server missing").stop()
    }

    // Admits each named entry in order (the first becomes controller),
    // assigns the slots that are asked for and leaves the rest as observers,
    // marks everyone ready, starts with `init_attributes` and drives
    // everyone through LOADED_GAME, so the server ends up InGame.
    pub fn start_match(
        &mut self,
        spec: &[(&str, Option<i8>)],
        init_attributes: &[u8],
    ) -> Vec<(PeerID, Guid)> {
        let controller = PeerID(1);
        let mut players = Vec::new();
        for (i, (name, slot)) in spec.iter().enumerate() {
            let peer = PeerID(1 + i);
            let guid = self.admit(peer, name);
            if let Some(slot) = slot {
                self.map_player_id_to_slot(controller, *slot, &guid);
            }
            players.push((peer, guid));
        }
        for (peer, guid) in &players {
            self.input(Input::Received {
                peer: *peer,
                msg: WireMessage::PreGameStatus(PreGameStatus {
                    guid: guid.clone(),
                    status: READY,
                }),
            });
        }
        self.input(Input::Received {
            peer: controller,
            msg: WireMessage::StartSettings(StartSettings {
                init_attributes: init_attributes.to_vec(),
            }),
        });
        assert!(
            matches!(self.server(), AnyServer::Loading(_)),
            "start_match: START_SETTINGS did not move the server into Loading"
        );
        for (peer, _) in &players {
            self.input(Input::Received {
                peer: *peer,
                msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
            });
        }
        assert!(
            matches!(self.server(), AnyServer::InGame(_)),
            "start_match: loading did not finish into InGame"
        );
        players
    }

    // Completes the newest running-game snapshot request with a one-chunk
    // payload, so a joiner may then report LOADED_GAME: the server refuses
    // one from a joiner it never handed a snapshot.
    pub fn serve_snapshot(&mut self) -> Vec<Effect> {
        self.serve_snapshot_bytes(vec![0u8; 4])
    }

    // For the pulls the server reads the turn of, which join snapshots are not.
    pub fn serve_snapshot_at(&mut self, turn: u32) -> Vec<Effect> {
        self.serve_snapshot_bytes(running_game(turn))
    }

    fn serve_snapshot_bytes(&mut self, data: Vec<u8>) -> Vec<Effect> {
        let (source, request_id) = self
            .log
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::Send {
                    peer,
                    msg: WireMessage::GamestateRequest(r),
                } if r.request_type == KIND_RUNNING_GAME => Some((*peer, r.request_id)),
                _ => None,
            })
            .expect("serve_snapshot: no snapshot was requested");
        self.input(Input::Received {
            peer: source,
            msg: WireMessage::GamestateResponse(GamestateResponse {
                request_id,
                length: data.len() as u32,
            }),
        });
        self.input(Input::Received {
            peer: source,
            msg: WireMessage::GamestateChunk(GamestateChunk { request_id, data }),
        })
    }

    pub fn log(&self) -> &[Effect] {
        &self.log
    }

    // Order preserved: per peer, effect order is the real wire order (4.1).
    pub fn sent_to(&self, peer: PeerID) -> Vec<WireMessage> {
        self.log
            .iter()
            .filter_map(|e| match e {
                Effect::Send { peer: p, msg } if *p == peer => Some(msg.clone()),
                _ => None,
            })
            .collect()
    }
}

// Broadcast fan-out has no defined order (4.1). Callers pass in whatever
// slice of effects they got back from one `input` call (or a wider slice of
// the log) and assert the recipient *set*, never a position within it.
pub fn recipients_of(
    effects: &[Effect],
    predicate: impl Fn(&WireMessage) -> bool,
) -> HashSet<PeerID> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send { peer, msg } if predicate(msg) => Some(*peer),
            _ => None,
        })
        .collect()
}

pub fn disconnects(effects: &[Effect]) -> Vec<(PeerID, DisconnectReason)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Disconnect { peer, reason } => Some((*peer, *reason)),
            _ => None,
        })
        .collect()
}

// None if nothing to `peer` matches; panics on more than one match, since
// every caller of this helper expects exactly one reply and a second one is
// itself worth failing loudly over.
pub fn expect_send<T>(
    effects: &[Effect],
    peer: PeerID,
    extract: impl Fn(&WireMessage) -> Option<T>,
) -> Option<T> {
    let mut found: Vec<T> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send { peer: p, msg } if *p == peer => extract(msg),
            _ => None,
        })
        .collect();
    assert!(
        found.len() <= 1,
        "expected at most one matching message sent to {peer:?}, got {}",
        found.len()
    );
    found.pop()
}

// A running game's state as a client frames it, with a stand-in for the
// simulation state the server never reads.
pub fn running_game(turn: u32) -> Vec<u8> {
    let mut payload = turn.to_le_bytes().to_vec();
    payload.extend_from_slice(&[7; 16]);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let mut framed = (payload.len() as u32).to_le_bytes().to_vec();
    framed.extend(encoder.finish().unwrap());
    framed
}

pub fn turn_sealed(peer: PeerID, turn: u32) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::TurnSealed(TurnSealed {
            turn,
            turn_length: 200,
        }),
    }
}

// Every server chat line in `effects` addressed to `peer`, in order.
pub fn chats_to(effects: &[Effect], peer: PeerID) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                peer: p,
                msg: WireMessage::Chat(c),
            } if *p == peer => Some(c.message.clone()),
            _ => None,
        })
        .collect()
}

// A PLAYER_COMMAND payload for `{type: "resign"}` in the structured-clone
// layout: OBJECT tag, one property, a Latin-1 key and a Latin-1 string value,
// every length a little-endian u32. Checked against the relay's own reader,
// so an encoding slip fails here rather than as a match that never ends.
pub fn resign_data() -> Vec<u8> {
    const OBJECT: u8 = 0x03;
    const STRING: u8 = 0x04;
    const LATIN1: u8 = 1;
    let latin1 = |data: &mut Vec<u8>, text: &str| {
        data.push(LATIN1);
        data.extend_from_slice(&(text.len() as u32).to_le_bytes());
        data.extend_from_slice(text.as_bytes());
    };
    let mut data = vec![OBJECT];
    data.extend_from_slice(&1u32.to_le_bytes());
    latin1(&mut data, "type");
    data.push(STRING);
    latin1(&mut data, "resign");
    assert_eq!(
        PlayerCommand::extract_command_type(&data).as_deref(),
        Some("resign"),
        "resign_data does not decode as a resign command"
    );
    data
}
