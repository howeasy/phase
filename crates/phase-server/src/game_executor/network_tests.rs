//! Exercise ordered Full publications through real WebSocket connections.
//! Codec peers retain an independent receiver baseline. Resync race tests use
//! instance-scoped gates and ordinary application FIFO fences, never sleeps or
//! heartbeat replies, to establish completion and publication ownership.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use server_core::protocol::{ClientMessage, DeckChoice, DeckData, ServerMessage};
use server_core::session::GameSession;
use server_core::FullSessionKey;

use crate::game_submission_tests::{
    basic_land_test_database, connect_and_hello, reconnect_started_game,
};
use crate::issue_4548_full_create_tests::{recv_server_message, send_test_message, spawn_server};
use crate::socket_test_hooks::run_async_test;

use super::GameExecutor;

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn receive_wire_json(socket: &mut ClientSocket) -> String {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let json = match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => text.to_string(),
            // Server snapshots are not subject to the 64 KiB C->S command cap.
            Message::Binary(bytes) => {
                crate::wire::decode_client_envelope(bytes.to_vec(), 16 * 1024 * 1024)
                    .await
                    .unwrap()
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            frame => panic!("expected an application reply on a live socket, got {frame:?}"),
        };
        return json;
    }
}

async fn receive_wire_message(socket: &mut ClientSocket) -> ServerMessage {
    serde_json::from_str(&receive_wire_json(socket).await).unwrap()
}

struct NativeTable {
    url: String,
    server: tokio::task::JoinHandle<()>,
    _directory: tempfile::TempDir,
    app: crate::AppState,
    key: FullSessionKey,
    tokens: Vec<String>,
}

impl NativeTable {
    async fn new() -> Self {
        Self::with_parallelism(NonZeroUsize::MIN).await
    }

    async fn with_parallelism(parallelism: NonZeroUsize) -> Self {
        Self::with_executor(GameExecutor::new(parallelism)).await
    }

    async fn with_executor(game_executor: GameExecutor) -> Self {
        let context = crate::ServerContext {
            game_executor,
            ..Default::default()
        };
        let (url, server, directory, app) = spawn_server(
            crate::ServerMode::Full,
            crate::draft_pools::DraftPools::default(),
            Arc::new(basic_land_test_database()),
            context,
        )
        .await;
        let (key, tokens) = {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let mut manager = app.sessions.lock().await;
            let (code, token) = manager
                .create_game_n_players_with_code(
                    deck.clone(),
                    Some(DeckChoice::DeckList(Box::new(names.clone()))),
                    "host".to_string(),
                    None,
                    4,
                    Default::default(),
                    Some(FormatConfig::free_for_all()),
                    None,
                )
                .unwrap();
            let session = manager.session_exclusive(&code).unwrap();
            let mut tokens = Vec::with_capacity(4);
            tokens.push(token);
            for name in ["guest-one", "guest-two", "guest-three"] {
                let (token, _) = session
                    .join_with_reservation(
                        deck.clone(),
                        Some(DeckChoice::DeckList(Box::new(names.clone()))),
                        name.to_string(),
                        None,
                    )
                    .unwrap();
                tokens.push(token);
            }
            session.set_card_names(app.db.card_names());
            session.start_game(&app.db).unwrap();
            let key = crate::bind_full_session_key(&app.game_db, &code).unwrap();
            crate::initialize_full_runtime(&app.game_db, session, key.clone()).unwrap();
            (key, tokens)
        };
        Self {
            url,
            server,
            _directory: directory,
            app,
            key,
            tokens,
        }
    }

    async fn connect(&self, player: PlayerId) -> ClientSocket {
        let mut socket = connect_and_hello(self.url.clone()).await;
        reconnect_started_game(
            &mut socket,
            &self.key.game_code,
            &self.tokens[usize::from(player.0)],
            self.key.clone(),
        )
        .await;
        socket
    }

    async fn connect_with_format(
        &self,
        player: PlayerId,
        format: Option<server_core::protocol::WireFormat>,
    ) -> ClientSocket {
        let Some(format) = format else {
            return self.connect(player).await;
        };
        let (mut socket, _) = tokio_tungstenite::connect_async(&self.url).await.unwrap();
        while !matches!(
            recv_server_message(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ) {}
        send_test_message(
            &mut socket,
            &crate::issue_4548_full_create_tests::test_client_hello(vec![format]),
            false,
        )
        .await;
        send_test_message(
            &mut socket,
            &ClientMessage::Reconnect {
                game_code: self.key.game_code.clone(),
                player_token: self.tokens[usize::from(player.0)].clone(),
                full_key: self.key.clone(),
            },
            true,
        )
        .await;
        loop {
            match receive_wire_message(&mut socket).await {
                ServerMessage::GameStarted { your_player, .. } => {
                    assert_eq!(your_player, player);
                    break;
                }
                ServerMessage::Error { message, .. } => {
                    panic!("formatted reconnect failed: {message}")
                }
                _ => {}
            }
        }
        socket
    }

    async fn keep_all(&self) {
        let session = crate::lock_session(&self.app.sessions, &self.key.game_code)
            .await
            .unwrap();
        let (lease, ()) = self
            .app
            .context
            .game_executor
            .with_session(session, |session| {
                for token in session.player_tokens.clone() {
                    session
                        .handle_action_with_card_db_outcome(
                            &token,
                            GameAction::MulliganDecision {
                                choice: MulliganChoice::Keep,
                            },
                            None,
                        )
                        .unwrap();
                    session.advance_state_revision();
                }
            })
            .await;
        drop(lease);
    }
}

struct CodecPeer {
    socket: ClientSocket,
    baseline: Option<state_codec::WireState>,
    gzip: bool,
}

struct CodecReply {
    value: serde_json::Value,
    kind: Option<state_codec::FrameKind>,
    text: bool,
}

impl CodecPeer {
    async fn next(&mut self) -> CodecReply {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let (json, text) = loop {
            match self.socket.next().await.unwrap().unwrap() {
                Message::Text(text) => break (text.to_string(), true),
                Message::Binary(bytes) => {
                    assert!(self.gzip, "plain peer must receive text");
                    break (
                        crate::wire::decode_client_envelope(bytes.to_vec(), 16 * 1024 * 1024)
                            .await
                            .unwrap(),
                        false,
                    );
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("expected live application frame, got {other:?}"),
            }
        };
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        match value["type"].as_str().unwrap() {
            "Full" | "Delta" => {
                let frame = state_codec::Frame::from_bytes(json.as_bytes()).unwrap();
                let decoded = state_codec::decode(self.baseline.as_ref(), &frame).unwrap();
                let value = serde_json::from_slice(decoded.as_bytes()).unwrap();
                self.baseline = Some(decoded);
                CodecReply {
                    value,
                    kind: Some(frame.kind()),
                    text,
                }
            }
            _ => {
                // Also validate the ordinary typed protocol, not just its tag.
                let _: ServerMessage = serde_json::from_str(&json).unwrap();
                CodecReply {
                    value,
                    kind: None,
                    text,
                }
            }
        }
    }

    async fn send(&mut self, message: &ClientMessage) {
        send_test_message(&mut self.socket, message, self.gzip).await;
    }

    async fn state(&mut self) -> CodecReply {
        loop {
            let reply = self.next().await;
            match reply.value["type"].as_str().unwrap() {
                "StateUpdate" | "GameStarted" => return reply,
                "Error" | "ActionFailed" | "ActionRejected" => {
                    panic!("state request rejected: {}", reply.value)
                }
                _ => {}
            }
        }
    }

    /// Subscription is an ordinary application command, unlike Pong. A state
    /// arriving before this reply is either explicitly drained or a failure.
    async fn fence(&mut self, allow_state: bool) {
        self.send(&ClientMessage::SubscribeLobby).await;
        loop {
            let reply = self.next().await;
            match reply.value["type"].as_str().unwrap() {
                "LobbyUpdate" => break,
                "ServerHello" => panic!("unexpected second/redundant confirmation"),
                "StateUpdate" | "GameStarted" => {
                    assert!(allow_state, "unexpected state: {}", reply.value)
                }
                "Error" => panic!("application FIFO fence rejected: {}", reply.value),
                _ => {}
            }
        }
    }

    async fn denial(&mut self) {
        loop {
            let reply = self.next().await;
            match reply.value["type"].as_str().unwrap() {
                "Error" => break,
                "StateUpdate" | "GameStarted" => {
                    panic!("denied route disclosed state: {}", reply.value)
                }
                _ => {}
            }
        }
    }

    async fn resync(&mut self) -> CodecReply {
        self.send(&ClientMessage::ResyncState).await;
        let reply = self.state().await;
        assert_eq!(reply.kind, Some(state_codec::FrameKind::Full));
        assert_eq!(reply.value["type"], "StateUpdate");
        assert!(reply.value.get("command_id").is_none());
        assert_eq!(reply.value["data"]["events"], serde_json::json!([]));
        assert_eq!(reply.value["data"]["log_entries"], serde_json::json!([]));
        reply
    }
}

async fn codec_hello(url: &str, gzip: bool, offers: Option<serde_json::Value>) -> CodecPeer {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut peer = CodecPeer {
        socket,
        baseline: None,
        gzip,
    };
    let initial = peer.next().await;
    assert!(initial.text, "initial ServerHello must be text");
    assert_eq!(initial.value["type"], "ServerHello");
    assert!(initial.value["data"].get("selected_state_codec").is_none());
    let full = initial.value["data"]["mode"] == "Full";
    if full {
        assert_eq!(
            initial.value["data"]["state_codecs"],
            serde_json::json!(["StateCodecV1"])
        );
    } else {
        assert!(initial.value["data"].get("state_codecs").is_none());
    }
    let mut hello = serde_json::to_value(crate::issue_4548_full_create_tests::test_client_hello(
        if gzip {
            vec![server_core::protocol::WireFormat::GzipEnvelopeV1]
        } else {
            Vec::new()
        },
    ))
    .unwrap();
    let selected = full
        && offers.as_ref().is_some_and(|offers| {
            offers
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry == "StateCodecV1")
        });
    if let Some(offers) = offers {
        hello["data"]["state_codecs"] = offers;
    }
    peer.socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .unwrap();
    if selected {
        let confirmation = peer.next().await;
        assert!(
            confirmation.text,
            "codec confirmation must remain text with gzip selected"
        );
        assert_eq!(confirmation.value["type"], "ServerHello");
        assert_eq!(
            confirmation.value["data"]["selected_state_codec"],
            "StateCodecV1"
        );
        let mut metadata = confirmation.value;
        metadata["data"]
            .as_object_mut()
            .unwrap()
            .remove("selected_state_codec");
        assert_eq!(
            metadata, initial.value,
            "both hellos share metadata authority"
        );
    }
    peer
}

async fn codec_seat(table: &NativeTable, player: PlayerId, gzip: bool) -> CodecPeer {
    let mut peer = codec_hello(&table.url, gzip, Some(serde_json::json!(["StateCodecV1"]))).await;
    peer.send(&ClientMessage::Reconnect {
        game_code: table.key.game_code.clone(),
        player_token: table.tokens[usize::from(player.0)].clone(),
        full_key: table.key.clone(),
    })
    .await;
    let started = peer.state().await;
    assert_eq!(started.kind, Some(state_codec::FrameKind::Full));
    assert_eq!(started.value["type"], "GameStarted");
    assert_projection(&started.value, &table.key, player);
    peer
}

async fn codec_spectator(table: &NativeTable, gzip: bool) -> CodecPeer {
    let mut peer = codec_hello(&table.url, gzip, Some(serde_json::json!(["StateCodecV1"]))).await;
    peer.send(&ClientMessage::SpectatorJoin {
        game_code: table.key.game_code.clone(),
    })
    .await;
    let started = peer.state().await;
    assert_eq!(started.kind, Some(state_codec::FrameKind::Full));
    assert_projection(&started.value, &table.key, crate::SPECTATOR_PLAYER_ID);
    peer
}

fn assert_projection(value: &serde_json::Value, key: &FullSessionKey, viewer: PlayerId) {
    let message: ServerMessage = serde_json::from_value(value.clone()).unwrap();
    match message {
        ServerMessage::GameStarted {
            full_key, state, ..
        }
        | ServerMessage::StateUpdate {
            full_key, state, ..
        } => {
            assert_eq!(full_key.as_ref(), Some(key));
            assert_eq!(state.viewer_projection, Some(viewer));
        }
        other => panic!("not projected state: {other:?}"),
    }
}

async fn current_action(table: &NativeTable) -> (PlayerId, GameAction) {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    assert!(
        session.ai_seats.is_empty(),
        "lease tests must not have AI work"
    );
    let actor = server_core::acting_players(&session.state)[0];
    let decision = crate::DecisionSnapshot::from_state(&session.state);
    let action = decision
        .legal_actions
        .iter()
        .find(|action| matches!(action, GameAction::PassPriority))
        .or_else(|| decision.legal_actions.first())
        .expect("engine-supplied legal action")
        .clone();
    (actor, action)
}

async fn resync_reference(
    table: &NativeTable,
    viewer: Option<PlayerId>,
) -> (serde_json::Value, serde_json::Value, u64) {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    let revision = session.state_revision;
    let state = serde_json::to_value(&session.state).unwrap();
    let reference =
        serde_json::to_value(crate::build_resync_state_message(&session, viewer).unwrap()).unwrap();
    (reference, state, revision)
}

#[test]
fn codec_confirmation_precedes_state() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            // Recompute the engine-supplied actor/action after each real action.
            for gzip in [false, true] {
                let (actor, action) = current_action(&table).await;
                let mut peer = codec_seat(&table, actor, gzip).await;
                peer.resync().await; // StateUpdate baseline, distinct from bootstrap.
                peer.fence(false).await;
                // Redundant hellos cannot reset the encoder's delivered baseline.
                peer.send(&crate::issue_4548_full_create_tests::test_client_hello(
                    Vec::new(),
                ))
                .await;
                peer.send(&ClientMessage::Action {
                    action: action.clone(),
                })
                .await;
                let update = peer.state().await;
                assert_eq!(update.kind, Some(state_codec::FrameKind::Delta));
                assert_projection(&update.value, &table.key, actor);
                peer.socket.close(None).await.unwrap();
            }
        })
        .await;
        table.server.abort();
        outcome.expect("negotiated production publication");
    });
}

#[test]
fn codec_absent_and_unknown_remain_legacy() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            for gzip in [false, true] {
                for offers in [None, Some(serde_json::json!(["FutureCodecV9"]))] {
                    let (actor, action) = current_action(&table).await;
                    let mut peer = codec_hello(&table.url, gzip, offers).await;
                    peer.fence(false).await; // no second hello before app completion
                    peer.send(&ClientMessage::Reconnect {
                        game_code: table.key.game_code.clone(),
                        player_token: table.tokens[usize::from(actor.0)].clone(),
                        full_key: table.key.clone(),
                    })
                    .await;
                    let started = peer.state().await;
                    assert!(started.kind.is_none());
                    assert_eq!(started.value["type"], "GameStarted");
                    peer.send(&ClientMessage::Action { action }).await;
                    let update = peer.state().await;
                    assert!(update.kind.is_none());
                    assert_eq!(update.value["type"], "StateUpdate");
                    assert_projection(&update.value, &table.key, actor);
                    peer.send(&ClientMessage::ResyncState).await;
                    loop {
                        let reply = peer.next().await;
                        if reply.value["type"] == "Error" {
                            break;
                        }
                        assert!(reply.kind.is_none());
                        assert_ne!(reply.value["type"], "StateUpdate");
                    }
                    peer.fence(false).await;
                    peer.socket.close(None).await.unwrap();
                }
            }
        })
        .await;
        table.server.abort();
        outcome.expect("legacy negotiated omissions");
    });
}

#[test]
fn codec_player_resync_full_then_delta() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let (actor, action) = current_action(&table).await;
            let mut peer = codec_seat(&table, actor, false).await;
            let other = PlayerId((actor.0 + 1) % 4);
            let mut other_peer = codec_seat(&table, other, false).await;
            let mut spectator = codec_spectator(&table, false).await;
            peer.resync().await;
            peer.fence(false).await;
            other_peer.fence(true).await;
            spectator.fence(true).await;
            let (reference, state_before, revision) = resync_reference(&table, Some(actor)).await;
            assert!(!reference["data"]["legal_actions"]
                .as_array()
                .unwrap()
                .is_empty());
            peer.baseline = None; // receiver recovery cannot use the lost baseline
            let recovered = peer.resync().await;
            assert_eq!(
                recovered.value, reference,
                "separately built envelopes compared structurally"
            );
            assert_projection(&recovered.value, &table.key, actor);
            let (_, state_after, after_revision) = resync_reference(&table, Some(actor)).await;
            assert_eq!(state_after, state_before);
            assert_eq!(after_revision, revision);
            other_peer.fence(false).await;
            spectator.fence(false).await;
            peer.send(&ClientMessage::Action { action }).await;
            let update = peer.state().await;
            assert_eq!(update.kind, Some(state_codec::FrameKind::Delta));
            assert!(update.value["data"]["state_revision"].as_u64().unwrap() > revision);
            for other in [&mut other_peer, &mut spectator] {
                let publication = other.state().await;
                assert!(
                    publication.value["data"]["state_revision"]
                        .as_u64()
                        .unwrap()
                        > revision
                );
            }
        })
        .await;
        table.server.abort();
        outcome.expect("recipient-local recovery and publication");
    });
}

#[test]
fn codec_spectator_resync_authority() {
    run_async_test(|| async {
        use crate::socket_test_hooks::ResyncPoint;
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let mut peer = codec_seat(&table, PlayerId(0), false).await;
            peer.send(&ClientMessage::SpectatorJoin {
                game_code: table.key.game_code.clone(),
            })
            .await;
            assert_projection(
                &peer.state().await.value,
                &table.key,
                crate::SPECTATOR_PLAYER_ID,
            );
            let seated = peer.resync().await;
            assert_projection(&seated.value, &table.key, PlayerId(0));
            // Reject the seat at its FINAL fence, not merely before projection.
            let built = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Built);
            peer.send(&ClientMessage::ResyncState).await;
            built.entered.notified().await;
            let old_route = table
                .app
                .connections
                .lock()
                .await
                .get_mut(&table.key.game_code)
                .unwrap()
                .remove(&PlayerId(0))
                .unwrap();
            built.release.notify_one();
            let public = peer.state().await;
            assert_eq!(public.kind, Some(state_codec::FrameKind::Full));
            assert_projection(&public.value, &table.key, crate::SPECTATOR_PLAYER_ID);
            let (reference, _, _) = resync_reference(&table, None).await;
            assert_eq!(
                public.value, reference,
                "spectator structural projection parity"
            );
            assert_eq!(public.value["data"]["legal_actions"], serde_json::json!([]));
            assert_eq!(
                public.value["data"]["rewind_targets"],
                serde_json::json!([])
            );
            assert!(public.value.get("command_id").is_none());
            assert_eq!(public.value["data"]["events"], serde_json::json!([]));
            assert_eq!(public.value["data"]["log_entries"], serde_json::json!([]));
            // Stale seat before build also falls back; it does not disclose hand data.
            peer.baseline = None;
            assert_projection(
                &peer.resync().await.value,
                &table.key,
                crate::SPECTATOR_PLAYER_ID,
            );
            let built = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Built);
            peer.send(&ClientMessage::ResyncState).await;
            built.entered.notified().await;
            let membership = table
                .app
                .game_spectators
                .lock()
                .await
                .remove(&table.key.game_code)
                .unwrap();
            built.release.notify_one();
            peer.denial().await;
            peer.fence(false).await;
            table
                .app
                .game_spectators
                .lock()
                .await
                .insert(table.key.game_code.clone(), membership);
            assert_projection(
                &peer.resync().await.value,
                &table.key,
                crate::SPECTATOR_PLAYER_ID,
            );
            // Keep the original seat invalid while observing ordinary public deltas.
            drop(old_route);
            let (actor, action) = current_action(&table).await;
            let mut acting = codec_seat(&table, actor, false).await;
            peer.fence(true).await;
            peer.resync().await;
            acting.send(&ClientMessage::Action { action }).await;
            let update = peer.state().await;
            assert_eq!(update.kind, Some(state_codec::FrameKind::Delta));
            assert_projection(&update.value, &table.key, crate::SPECTATOR_PLAYER_ID);
        })
        .await;
        table.server.abort();
        outcome.expect("live seat precedence and independently authorized spectator fallback");
    });
}

#[test]
fn codec_resync_rejects_stale_routes() {
    run_async_test(|| async {
        use crate::socket_test_hooks::ResyncPoint;
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let mut unattached =
                codec_hello(&table.url, false, Some(serde_json::json!(["StateCodecV1"]))).await;
            unattached.send(&ClientMessage::ResyncState).await;
            unattached.denial().await;
            unattached.fence(false).await;
            // A denied request must release admission so this socket can recover after attach.
            unattached
                .send(&ClientMessage::Reconnect {
                    game_code: table.key.game_code.clone(),
                    player_token: table.tokens[0].clone(),
                    full_key: table.key.clone(),
                })
                .await;
            unattached.state().await;
            unattached.resync().await;
            let mut other = codec_seat(&table, PlayerId(1), false).await;
            unattached.fence(true).await;
            let built = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Built);
            unattached.send(&ClientMessage::ResyncState).await;
            built.entered.notified().await;
            {
                let mut routes = table.app.connections.lock().await;
                let players = routes.get_mut(&table.key.game_code).unwrap();
                let replacement = players.get(&PlayerId(1)).unwrap().clone();
                players.insert(PlayerId(0), replacement);
            }
            built.release.notify_one();
            unattached.denial().await;
            unattached.fence(false).await;
            other.fence(true).await;
            let mut current = codec_seat(&table, PlayerId(0), false).await;
            current.resync().await;
            unattached.send(&ClientMessage::ResyncState).await;
            unattached.denial().await;
            unattached.fence(false).await;
            // A current channel is insufficient when its token no longer resolves
            // to that seat in the SAME session.
            {
                let mut session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                session.player_tokens.swap(0, 1);
            }
            current.send(&ClientMessage::ResyncState).await;
            current.denial().await;
            current.fence(false).await;
            {
                let mut session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                session.player_tokens.swap(0, 1);
            }
            current.resync().await;
            assert!(table
                .app
                .sessions
                .lock()
                .await
                .remove_game(&table.key.game_code));
            current.send(&ClientMessage::ResyncState).await;
            current.denial().await;
            current.fence(false).await;
        })
        .await;
        table.server.abort();
        outcome.expect("stale routes cannot disclose projected state");
    });
}

#[test]
fn codec_resync_burst_coalesces() {
    run_async_test(|| async {
        use crate::socket_test_hooks::ResyncPoint;
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let work = table.app.context.socket_hooks.arm_parser_work();
            let mut peer = codec_seat(&table, PlayerId(0), false).await;
            work.wait_completed(2).await;
            let dispatch = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Dispatch);
            let built = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Built);
            let encoding = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Encoding);
            peer.send(&ClientMessage::ResyncState).await;
            dispatch.entered.notified().await;
            for _ in 0..4 {
                peer.send(&ClientMessage::ResyncState).await;
            }
            work.wait_completed(7).await;
            dispatch.release.notify_one();
            built.entered.notified().await;
            for _ in 0..4 {
                peer.send(&ClientMessage::ResyncState).await;
            }
            work.wait_completed(11).await;
            built.release.notify_one();
            encoding.entered.notified().await;
            for _ in 0..4 {
                peer.send(&ClientMessage::ResyncState).await;
            }
            work.wait_completed(15).await;
            encoding.release.notify_one();
            assert_eq!(peer.state().await.kind, Some(state_codec::FrameKind::Full));
            peer.fence(false).await; // any extra pending FULL fails before sentinel
            peer.resync().await; // encoding completion makes a later retry usable
            peer.send(&ClientMessage::Ping { timestamp: 901 }).await;
            loop {
                let reply = peer.next().await;
                if reply.value["type"] == "Pong" {
                    break;
                }
            }
            peer.fence(false).await;
        })
        .await;
        table.server.abort();
        outcome.expect("one pending resync through dispatch/mailbox/encoding");
    });
}

#[test]
fn codec_resync_holds_publication_lease() {
    run_async_test(|| async {
        use crate::socket_test_hooks::ResyncPoint;
        let table = NativeTable::new().await;
        let outcome =
            tokio::time::timeout(Duration::from_secs(20), async {
                table.keep_all().await;
                let (actor, action) = current_action(&table).await;
                let mut acting = codec_seat(&table, actor, false).await;
                let mut requester = codec_spectator(&table, false).await;
                acting.fence(true).await;
                requester.resync().await;
                requester.fence(false).await;
                let handle = table
                    .app
                    .sessions
                    .lock()
                    .await
                    .session(&table.key.game_code)
                    .unwrap();
                assert!(
                    handle.clone().try_lock_owned().is_ok(),
                    "drained fixture has no background lease owner"
                );
                // Ownership sub-case: NO competing command exists at either assertion.
                let built = table
                    .app
                    .context
                    .socket_hooks
                    .arm_resync(ResyncPoint::Built);
                let enqueued = table
                    .app
                    .context
                    .socket_hooks
                    .arm_resync(ResyncPoint::Enqueued);
                requester.send(&ClientMessage::ResyncState).await;
                built.entered.notified().await;
                let (key, revision) = built.snapshot.lock().clone().unwrap();
                assert_eq!(key, table.key);
                assert!(
                    handle.clone().try_lock_owned().is_err(),
                    "resync owns after-build lease"
                );
                built.release.notify_one();
                enqueued.entered.notified().await;
                let full = requester.state().await; // prove real enqueue BEFORE ownership check
                assert_eq!(full.kind, Some(state_codec::FrameKind::Full));
                assert_eq!(full.value["data"]["state_revision"], revision);
                assert!(handle.clone().try_lock_owned().is_err(),
                "same resync lease must survive enqueue; both early-release mutations fail here");
                assert_eq!(
                    *enqueued.snapshot.lock(),
                    Some((table.key.clone(), revision))
                );
                enqueued.release.notify_one();
                requester.fence(false).await;
                // Separate ordering sub-case: independent socket enters BEFORE preflight.
                let built = table
                    .app
                    .context
                    .socket_hooks
                    .arm_resync(ResyncPoint::Built);
                let preflight = table
                    .app
                    .context
                    .socket_hooks
                    .arm_resync(ResyncPoint::ActionPreflight);
                requester.send(&ClientMessage::ResyncState).await;
                built.entered.notified().await;
                let (_, revision) = built.snapshot.lock().clone().unwrap();
                acting.send(&ClientMessage::Action { action }).await;
                preflight.entered.notified().await;
                preflight.release.notify_one(); // action proceeds toward its first session acquisition
                built.release.notify_one();
                let full = requester.state().await;
                assert_eq!(full.kind, Some(state_codec::FrameKind::Full));
                assert_eq!(full.value["data"]["state_revision"], revision);
                let publication = requester.state().await;
                assert!(
                    publication.value["data"]["state_revision"]
                        .as_u64()
                        .unwrap()
                        > revision
                );
                let acting_publication = acting.state().await;
                assert!(
                    acting_publication.value["data"]["state_revision"]
                        .as_u64()
                        .unwrap()
                        > revision
                );
                requester.fence(false).await;
            })
            .await;
        table.server.abort();
        outcome.expect("resync retains publication lease through final enqueue");
    });
}

#[test]
fn codec_lobby_only_rejects_resync() {
    run_async_test(|| async {
        let (url, server, _directory, _app) = spawn_server(
            crate::ServerMode::LobbyOnly,
            crate::draft_pools::DraftPools::default(),
            Arc::new(basic_land_test_database()),
            crate::ServerContext::default(),
        )
        .await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let mut peer =
                codec_hello(&url, false, Some(serde_json::json!(["StateCodecV1"]))).await;
            peer.send(&ClientMessage::ResyncState).await;
            peer.denial().await;
            peer.fence(false).await;
        })
        .await;
        server.abort();
        outcome.expect("lobby capability remains absent and unsupported");
    });
}

fn enable_ai(session: &mut GameSession, players: impl IntoIterator<Item = PlayerId>) {
    for player in players {
        session.ai_seats.insert(player);
        session.ai_configs.insert(
            player,
            create_config_for_players(AiDifficulty::Easy, Platform::Native, 4),
        );
    }
}

#[test]
fn queued_reconnect_before_departure_keeps_contest_presence_causal() {
    run_async_test(|| async {
        use engine::types::contest::{
            ContestConfig, ContestKind, ContestPhase, ContestState, ContestantConnection,
        };
        use engine::types::game_state::WaitingFor;

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let mut old = table.connect(PlayerId(0)).await;
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, ()) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    let config = ContestConfig {
                        countdown_ms: 0,
                        inactivity_timeout_ms: 60_000,
                        reconnect_grace_ms: 100,
                        settle_window_ms: 0,
                        max_active_ms: 60_000,
                    };
                    session.contest.policy.config = config;
                    session.state.phase = engine::types::phase::Phase::PreCombatMain;
                    session.state.active_player = PlayerId(0);
                    let source_id = *session.state.objects.keys().next().unwrap();
                    session.state.waiting_for = WaitingFor::PhysicalContest {
                        contest: Box::new(ContestState::new(
                            91,
                            ContestKind::Staring,
                            PlayerId(0),
                            PlayerId(1),
                            config,
                        )),
                        source_id,
                    };
                    session.tick_contest_at(std::time::Instant::now());
                    session.drain_contest_outbox();
                    // Keep the autonomous driver from consuming either edge
                    // until the replacement has completed its real bootstrap.
                    assert!(session.claim_contest_driver());
                })
                .await;
            drop(lease);

            let work = table.app.context.socket_hooks.arm_parser_work();
            let mut replacement = connect_and_hello(table.url.clone()).await;
            work.wait_completed(1).await;
            let lobby = table.app.lobby.lock().await;
            send_test_message(&mut replacement, &ClientMessage::SubscribeLobby, false).await;
            send_test_message(
                &mut replacement,
                &ClientMessage::Reconnect {
                    game_code: table.key.game_code.clone(),
                    player_token: table.tokens[0].clone(),
                    full_key: table.key.clone(),
                },
                false,
            )
            .await;
            // Decoder completion, not Pong, proves that both ordinary commands
            // have been retained in FIFO order behind the held lobby dispatch.
            work.wait_completed(3).await;
            // Keep the host's millisecond receipt/death stamps distinct without
            // relying on a race between worker scheduling and socket cleanup.
            tokio::time::sleep(Duration::from_millis(5)).await;
            old.close(None).await.unwrap();
            drop(old);
            loop {
                let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let away = !lease.connected[0];
                drop(lease);
                let recorded = table
                    .app
                    .sessions
                    .lock()
                    .await
                    .reconnect
                    .is_disconnected(&table.key.game_code, PlayerId(0));
                let unbound = table
                    .app
                    .connections
                    .lock()
                    .await
                    .get(&table.key.game_code)
                    .is_none_or(|players| !players.contains_key(&PlayerId(0)));
                if away && recorded && unbound {
                    break;
                }
                tokio::task::yield_now().await;
            }
            drop(lobby);
            loop {
                match receive_wire_message(&mut replacement).await {
                    ServerMessage::GameStarted { your_player, .. } => {
                        assert_eq!(your_player, PlayerId(0));
                        break;
                    }
                    ServerMessage::Error { message, .. } => panic!("replacement: {message}"),
                    _ => {}
                }
            }

            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, before_tick) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    assert!(session.connected[0]);
                    session.release_contest_driver();
                    session.state_revision
                })
                .await;
            drop(lease);
            crate::ensure_contest_driver(
                &table.app.context.game_executor,
                &table.app.sessions,
                &table.app.connections,
                &table.app.draft_sessions,
                &table.app.game_db,
                &table.app.game_spectators,
                &table.app.lobby,
                &table.app.lobby_subscribers,
                &table.key.game_code,
            )
            .await;
            let mut departure = None;
            let (departed_at, resumed_revision) = loop {
                match receive_wire_message(&mut replacement).await {
                    ServerMessage::StateUpdate {
                        state_revision,
                        state,
                        ..
                    } if state_revision > before_tick => {
                        let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for else {
                            panic!("the accepted replacement must not forfeit");
                        };
                        assert_eq!(contest.id, 91);
                        match contest.contestants[0].connection {
                            ContestantConnection::Disconnected { since_ms } => {
                                assert_eq!(contest.paused_since_ms, Some(since_ms));
                                departure = Some((state_revision, since_ms));
                            }
                            ContestantConnection::Connected => {
                                let Some((departure_revision, departed_at)) = departure else {
                                    continue;
                                };
                                assert!(state_revision > departure_revision);
                                assert!(contest.clock_ms >= departed_at);
                                assert_eq!(contest.paused_since_ms, None);
                                break (departed_at, state_revision);
                            }
                        }
                    }
                    ServerMessage::Error { message, .. } => {
                        panic!("contest publication: {message}");
                    }
                    _ => {}
                }
            };
            // A revision bump alone could precede the driver. Ordered departure
            // AND return snapshots positively prove that both queued edges ran.
            {
                let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let WaitingFor::PhysicalContest { contest, .. } = &lease.state.waiting_for else {
                    panic!("the published return must remain active");
                };
                assert!(lease.state_revision >= resumed_revision);
                assert!(lease.connected[0]);
                assert!(contest.clock_ms >= departed_at);
                assert_eq!(
                    contest.contestants[0].connection,
                    ContestantConnection::Connected
                );
                assert_eq!(contest.paused_since_ms, None);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let WaitingFor::PhysicalContest { contest, .. } = &lease.state.waiting_for else {
                panic!("no stale grace may forfeit the replacement later");
            };
            assert_eq!(contest.phase, ContestPhase::Active);
            assert_eq!(
                contest.contestants[0].connection,
                ContestantConnection::Connected
            );
        })
        .await;
        table.server.abort();
        outcome.expect("queued receipt before old-route cleanup must remain a live contest seat");
    });
}

#[test]
fn fresh_reconnect_never_replays_ai_history_older_than_its_bootstrap() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut host = table.connect(PlayerId(0)).await;
            let mut old = table.connect(PlayerId(3)).await;
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, initial_revision) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    enable_ai(session, [PlayerId(1), PlayerId(2)]);
                    session.state_revision
                })
                .await;
            drop(lease);
            let gate = table.app.context.game_executor.arm_historical_publication();
            send_test_message(
                &mut host,
                &ClientMessage::Action {
                    action: GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                },
                false,
            )
            .await;
            gate.entered.notified().await;

            let mut returning = connect_and_hello(table.url.clone()).await;
            send_test_message(
                &mut returning,
                &ClientMessage::Reconnect {
                    game_code: table.key.game_code.clone(),
                    player_token: table.tokens[3].clone(),
                    full_key: table.key.clone(),
                },
                false,
            )
            .await;
            send_test_message(
                &mut returning,
                &ClientMessage::Ping { timestamp: 91 },
                false,
            )
            .await;
            let mut bootstrap = None;
            let mut updates = Vec::new();
            // Allow an early bootstrap in the negative control: the defect is
            // the older history delivered AFTER it, not merely the lease gap.
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::Pong { timestamp: 91 } => break,
                    ServerMessage::GameStarted {
                        state_revision,
                        legal_actions,
                        ..
                    } => {
                        bootstrap = Some((state_revision, legal_actions));
                    }
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("reconnect: {message}"),
                    _ => {}
                }
            }
            gate.release.notify_one();
            while bootstrap.is_none() {
                match recv_server_message(&mut returning).await {
                    ServerMessage::GameStarted {
                        state_revision,
                        legal_actions,
                        ..
                    } => {
                        bootstrap = Some((state_revision, legal_actions));
                    }
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("reconnect: {message}"),
                    _ => {}
                }
            }
            let (revision, legal_actions) = bootstrap.unwrap();
            assert!(
                revision >= initial_revision + 3,
                "the real human action must drive several AI transitions"
            );
            // Reading the initiator's final frame proves that every historical
            // transition has been enqueued before the drain delimiter below.
            loop {
                match recv_server_message(&mut host).await {
                    ServerMessage::StateUpdate { state_revision, .. }
                        if state_revision >= revision =>
                    {
                        break
                    }
                    ServerMessage::ActionFailed { message } => panic!("human action: {message}"),
                    ServerMessage::ActionRejected { rejection } => {
                        panic!("human action: {rejection:?}")
                    }
                    _ => {}
                }
            }
            send_test_message(&mut returning, &ClientMessage::SubscribeLobby, false).await;
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::LobbyUpdate { .. } => break,
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("drain: {message}"),
                    _ => {}
                }
            }
            assert!(
                updates.iter().all(|seen| *seen >= revision),
                "a fresh socket rolled back after GameStarted {revision}: {updates:?}"
            );
            old.close(None).await.unwrap();
            let action = legal_actions
                .into_iter()
                .find(|action| {
                    matches!(
                        action,
                        GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep
                        }
                    )
                })
                .expect("the reconnected human still owes its opening decision");
            send_test_message(&mut returning, &ClientMessage::Action { action }, false).await;
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::StateUpdate {
                        state,
                        state_revision,
                        ..
                    } => {
                        assert!(state_revision > revision);
                        if let engine::types::game_state::WaitingFor::MulliganDecision {
                            pending,
                            ..
                        } = state.waiting_for
                        {
                            assert!(pending.iter().all(|entry| entry.player != PlayerId(3)));
                        }
                        break;
                    }
                    ServerMessage::ActionFailed { message } => panic!("new owner: {message}"),
                    ServerMessage::ActionRejected { rejection } => {
                        panic!("new owner: {rejection:?}")
                    }
                    _ => {}
                }
            }
        })
        .await;
        table.server.abort();
        outcome.expect("gated AI publication or fresh reconnect stalled");
    });
}

#[derive(Clone, Copy)]
enum EliminationPublication {
    Concede,
    AiReconnect,
}

#[test]
fn elimination_metadata_survives_native_concession() {
    assert_elimination_publication(EliminationPublication::Concede);
}

#[test]
fn elimination_metadata_survives_ai_reconnect_fanout() {
    assert_elimination_publication(EliminationPublication::AiReconnect);
}

fn assert_elimination_publication(path: EliminationPublication) {
    run_async_test(move || async move {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            table.keep_all().await;
            let (mut viewer, _returning) = match path {
                EliminationPublication::Concede => (table.connect(PlayerId(0)).await, None),
                EliminationPublication::AiReconnect => {
                    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let (lease, (ai, returning, observer)) = table
                        .app
                        .context
                        .game_executor
                        .with_session(session, |session| {
                            let token = session.player_tokens[2].clone();
                            session
                                .handle_action_with_card_db_outcome(
                                    &token,
                                    GameAction::Concede {
                                        player_id: PlayerId(2),
                                    },
                                    None,
                                )
                                .unwrap();
                            session.advance_state_revision();
                            let ai = server_core::acting_players(&session.state)[0];
                            let mut humans = [PlayerId(0), PlayerId(1), PlayerId(3)]
                                .into_iter()
                                .filter(|player| *player != ai);
                            (ai, humans.next().unwrap(), humans.next().unwrap())
                        })
                        .await;
                    drop(lease);
                    let observer_socket = table.connect(observer).await;
                    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let (lease, ()) = table
                        .app
                        .context
                        .game_executor
                        .with_session(session, move |session| {
                            enable_ai(session, [ai]);
                        })
                        .await;
                    drop(lease);
                    let returning_socket = table.connect(returning).await;
                    (observer_socket, Some(returning_socket))
                }
            };
            let _conceding = if matches!(path, EliminationPublication::Concede) {
                let mut socket = table.connect(PlayerId(2)).await;
                send_test_message(&mut socket, &ClientMessage::Concede, false).await;
                Some(socket)
            } else {
                None
            };
            loop {
                match recv_server_message(&mut viewer).await {
                    ServerMessage::StateUpdate {
                        eliminated_players, ..
                    } => {
                        assert_eq!(
                            eliminated_players,
                            vec![PlayerId(2)],
                            "the client must see the conceded seat eliminated after publication"
                        );
                        break;
                    }
                    ServerMessage::ActionFailed { message } => panic!("concede: {message}"),
                    ServerMessage::Error { message, .. } => panic!("publication: {message}"),
                    _ => {}
                }
            }
        })
        .await;
        table.server.abort();
        outcome.expect("the real elimination publication stalled");
    });
}

async fn receive_command_frame(socket: &mut ClientSocket) -> serde_json::Value {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        match socket
            .next()
            .await
            .expect("live command socket")
            .expect("valid frame")
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            frame => panic!("command test expected plain JSON, got {frame:?}"),
        }
    }
}

async fn send_command_frame(socket: &mut ClientSocket, action: &GameAction, command_id: u64) {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "Action",
                "command_id": command_id,
                "data": { "action": action },
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

fn owns_command_completion(frame: &serde_json::Value, command_id: u64) -> bool {
    frame.get("command_id").and_then(serde_json::Value::as_u64) == Some(command_id)
}

#[test]
fn four_human_mulligans_settle_only_the_originating_commands() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut sockets = Vec::with_capacity(4);
            let mut keeps = Vec::with_capacity(4);
            for player in 0..4 {
                let mut socket = connect_and_hello(table.url.clone()).await;
                send_test_message(
                    &mut socket,
                    &ClientMessage::Reconnect {
                        game_code: table.key.game_code.clone(),
                        player_token: table.tokens[player].clone(),
                        full_key: table.key.clone(),
                    },
                    false,
                )
                .await;
                loop {
                    let frame = receive_command_frame(&mut socket).await;
                    if frame["type"] == "GameStarted" {
                        assert_eq!(frame["data"]["your_player"], player);
                        let legal: Vec<GameAction> =
                            serde_json::from_value(frame["data"]["legal_actions"].clone()).unwrap();
                        keeps.push(
                            legal
                                .into_iter()
                                .find(|action| {
                                    matches!(
                                        action,
                                        GameAction::MulliganDecision {
                                            choice: MulliganChoice::Keep
                                        }
                                    )
                                })
                                .expect("each human receives its own opening decision"),
                        );
                        break;
                    }
                }
                sockets.push(socket);
            }

            // All four clients have submitted, but none can have applied yet.
            // The first publication is therefore ambient for three clients.
            let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            for (player, socket) in sockets.iter_mut().enumerate() {
                send_command_frame(socket, &keeps[player], 100 + player as u64).await;
            }
            drop(lease);
            let mut pending = [true; 4];
            let mut own_completions = [0; 4];
            let mut first_revision = None;
            let mut ambient = 0;
            for (player, socket) in sockets.iter_mut().enumerate() {
                loop {
                    let frame = receive_command_frame(socket).await;
                    if frame["type"] != "StateUpdate" {
                        continue;
                    }
                    let revision = frame["data"]["state_revision"].as_u64().unwrap();
                    assert_eq!(*first_revision.get_or_insert(revision), revision);
                    let is_own = frame.get("command_id").is_some();
                    if is_own {
                        assert!(owns_command_completion(&frame, 100 + player as u64));
                        own_completions[player] += 1;
                    } else {
                        ambient += 1;
                    }
                    if owns_command_completion(&frame, 100 + player as u64) {
                        pending[player] = false;
                    }
                    // Model the actual client bug: releasing the slot on an
                    // ambient frame permits its still-legal Keep to be sent a
                    // second time, behind this socket's original command.
                    let legal: Vec<GameAction> =
                        serde_json::from_value(frame["data"]["legal_actions"].clone()).unwrap();
                    if !pending[player] && legal.iter().any(|action| action == &keeps[player]) {
                        send_command_frame(socket, &keeps[player], 200 + player as u64).await;
                    }
                    break;
                }
            }
            assert_eq!(
                ambient, 3,
                "simultaneous decisions must expose the peer-update window"
            );
            let mut rejections = Vec::new();
            for socket in &mut sockets {
                // Application FIFO, not Pong, is the authoritative drain fence.
                send_test_message(socket, &ClientMessage::SubscribeLobby, false).await;
            }
            for (player, socket) in sockets.iter_mut().enumerate() {
                loop {
                    let frame = receive_command_frame(socket).await;
                    match frame["type"].as_str().unwrap() {
                        "LobbyUpdate" => break,
                        "StateUpdate" => {
                            if let Some(id) =
                                frame.get("command_id").and_then(serde_json::Value::as_u64)
                            {
                                assert_eq!(
                                    id,
                                    100 + player as u64,
                                    "a peer must never receive this echo"
                                );
                                pending[player] = false;
                                own_completions[player] += 1;
                            }
                        }
                        "ActionRejected" | "ActionFailed" | "Error" => rejections.push(frame),
                        _ => {}
                    }
                }
            }
            assert!(
                rejections.is_empty(),
                "ambient ACKs replayed opening commands: {rejections:?}"
            );
            assert_eq!(pending, [false; 4]);
            assert_eq!(own_completions, [1; 4]);
            for socket in &mut sockets {
                socket.close(None).await.unwrap();
            }
        })
        .await;
        table.server.abort();
        outcome.expect("four-human command correlation must finish");
    });
}

async fn queue_named_revision(table: &NativeTable, name: String) -> u64 {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    let db = table.app.game_db.clone();
    let executor = table.app.context.game_executor.clone();
    let persistence_executor = executor.clone();
    let (lease, revision) = executor
        .with_session(session, move |session| {
            session.display_names[0] = name;
            let revision = session.advance_state_revision();
            crate::persist_full_session_async(&persistence_executor, &db, session);
            revision
        })
        .await;
    drop(lease);
    revision
}

async fn persistence_pool_probe(table: &NativeTable) -> Arc<str> {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    let (lease, pools) = table
        .app
        .context
        .game_executor
        .with_session(session, |session| {
            session.full_persist_snapshot().unwrap().deck_pools_json
        })
        .await;
    drop(lease);
    pools
}

#[test]
fn pending_persistence_retains_one_snapshot_and_saves_the_latest_revision() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let pools = persistence_pool_probe(&table).await;
            let baseline = Arc::strong_count(&pools);
            let writes = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(tokio::sync::Notify::new());
            let completed = Arc::new(tokio::sync::Notify::new());
            let (release, blocked) = std::sync::mpsc::channel();
            let blocked = parking_lot::Mutex::new(Some(blocked));
            table.app.game_db.set_full_save_io_hook(Arc::new({
                let writes = writes.clone();
                let entered = entered.clone();
                let completed = completed.clone();
                move |_, _| {
                    writes.fetch_add(1, Ordering::SeqCst);
                    if let Some(blocked) = blocked.lock().take() {
                        entered.notify_one();
                        blocked.recv().unwrap();
                    } else {
                        completed.notify_one();
                    }
                }
            }));
            queue_named_revision(&table, "first".to_string()).await;
            entered.notified().await;
            let mut latest = 0;
            for revision in 1..=100 {
                latest = queue_named_revision(&table, format!("latest-{revision}")).await;
            }
            // Each eager pending FullPersistSnapshot retains this real shared
            // pool buffer. One prepared write may retain it; 100 queued saves
            // must not retain 100 complete game states behind that write.
            let retained = Arc::strong_count(&pools);
            release.send(()).unwrap();
            completed.notified().await;
            let db = table.app.game_db.clone();
            let loaded =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            let saved = loaded
                .into_iter()
                .find(|saved| saved.key == table.key)
                .unwrap();
            assert!(
                retained <= baseline + 1,
                "pending saves retained {} extra game snapshots behind one writer",
                retained.saturating_sub(baseline),
            );
            assert_eq!(saved.mutation_revision, latest);
            assert_eq!(saved.persisted.display_names[0], "latest-100");
            assert_eq!(
                writes.load(Ordering::SeqCst),
                2,
                "only the in-flight and latest saves should reach SQLite"
            );
        })
        .await;
        table.server.abort();
        outcome.expect("coalesced persistence must drain");
    });
}

#[test]
fn terminal_retirement_cancels_pending_saves_before_serialization() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let pools = persistence_pool_probe(&table).await;
            let baseline = Arc::strong_count(&pools);
            let serialized = Arc::new(AtomicUsize::new(0));
            table.app.game_db.set_full_save_hook(Arc::new({
                let serialized = serialized.clone();
                move |_| {
                    serialized.fetch_add(1, Ordering::SeqCst);
                }
            }));
            let held_io = table.app.context.game_executor.admit_persistence_io().await;
            for revision in 1..=20 {
                queue_named_revision(&table, format!("pending-{revision}")).await;
            }
            let mut sockets = Vec::with_capacity(4);
            for player in 0..4 {
                sockets.push(table.connect(PlayerId(player)).await);
            }
            for socket in sockets.iter_mut().take(3) {
                send_test_message(socket, &ClientMessage::Concede, false).await;
                loop {
                    let frame = receive_command_frame(socket).await;
                    match frame["type"].as_str().unwrap() {
                        "Conceded" => break,
                        "Error" | "ActionFailed" | "ActionRejected" => {
                            panic!("concede refused: {frame}")
                        }
                        _ => {}
                    }
                }
            }
            loop {
                let frame = receive_command_frame(&mut sockets[3]).await;
                if frame["type"] == "TerminalResult" {
                    break;
                }
            }
            drop(held_io);
            while Arc::strong_count(&pools) > baseline {
                tokio::task::yield_now().await;
            }
            let db = table.app.game_db.clone();
            let active =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            assert!(active.iter().all(|snapshot| snapshot.key != table.key));
            assert_eq!(
                serialized.load(Ordering::SeqCst),
                0,
                "retired pending snapshots must not be serialized and rejected later"
            );
        })
        .await;
        table.server.abort();
        outcome.expect("retirement must cancel the persistence backlog");
    });
}

async fn assert_quota_refusal_backlog_is_bounded(payload: serde_json::Value) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let table = NativeTable::new().await;
    let gate = table.app.context.socket_hooks.arm_bootstrap();
    let outcome = tokio::time::timeout(Duration::from_secs(8), async {
        let mut socket = connect_and_hello(table.url.clone()).await;
        send_test_message(
            &mut socket,
            &ClientMessage::Reconnect {
                game_code: table.key.game_code.clone(),
                player_token: table.tokens[0].clone(),
                full_key: table.key.clone(),
            },
            false,
        )
        .await;
        // This encoder barrier holds ALL application replies before the kernel
        // sink, so the bound does not depend on platform socket-buffer capacity.
        gate.entered.notified().await;
        for _ in 0..crate::RATE_LIMIT_MESSAGES {
            send_test_message(&mut socket, &ClientMessage::SubscribeLobby, false).await;
        }
        for id in 0..crate::socket_mailbox::MAX_PENDING_QUOTA_REFUSALS {
            let mut frame = payload.clone();
            frame["command_id"] = serde_json::json!(100 + id);
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 91 }, false).await;
        loop {
            if matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 91 }
            ) {
                break;
            }
        }
        let mut frame = payload;
        frame["command_id"] = serde_json::json!(999);
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Close(Some(close)) => {
                    assert_eq!(u16::from(close.code), 1013);
                    break;
                }
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) => {}
                frame => panic!("unexpected refusal transport frame: {frame:?}"),
            }
        }
        assert_eq!(
            table
                .app
                .context
                .metrics
                .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaRefusalOverflow),
            1,
        );
    })
    .await;
    gate.release.notify_one();
    table.server.abort();
    outcome.expect("unflushed quota replies must close with a bounded backlog");
}

#[test]
fn correlated_quota_refusals_bound_the_slow_consumer_backlog() {
    run_async_test(|| async {
        assert_quota_refusal_backlog_is_bounded(serde_json::json!({
            "type": "Action", "data": { "action": { "type": "PassPriority" } },
        }))
        .await;
    });
}

#[test]
fn malformed_correlated_quota_refusals_bound_the_slow_consumer_backlog() {
    run_async_test(|| async {
        assert_quota_refusal_backlog_is_bounded(serde_json::json!({
            "type": "Action", "data": { "action": { "type": "NotAnAction" } },
        }))
        .await;
    });
}

#[test]
fn captured_checkpoints_keep_fifo_admission_during_sustained_mutation() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let executor = table.app.context.game_executor.clone();
            let held_io = executor.admit_persistence_io().await;
            let queued = executor.arm_persistence_admission();
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let db = table.app.game_db.clone();
            let persistence_executor = executor.clone();
            let (lease, captured_revision) = executor
                .with_session(session, move |session| {
                    session.advance_state_revision();
                    crate::persist_full_session_async(&persistence_executor, &db, session);
                    // A revision advance without another save request must not
                    // invalidate the already-requested checkpoint.
                    session.display_names[0] = "current-without-another-request".to_string();
                    session.advance_state_revision()
                })
                .await;
            drop(lease);
            queued.notified().await;

            let observation = executor.admit_persistence_io();
            tokio::pin!(observation);
            tokio::select! {
                biased;
                _ = &mut observation => panic!("the test still holds I/O admission"),
                _ = tokio::task::yield_now() => {}
            }
            for revision in 0..32 {
                queue_named_revision(&table, format!("still-mutating-{revision}")).await;
            }
            drop(held_io);
            // This waiter was queued after the checkpoint. It can acquire only
            // after that checkpoint commits, not after an abandon-and-requeue.
            let io = observation.await;
            let db = table.app.game_db.clone();
            let loaded =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            let saved = loaded
                .into_iter()
                .find(|save| save.key == table.key)
                .unwrap();
            let actual = saved.mutation_revision;
            let names = saved.persisted.display_names;
            drop(io);
            assert_eq!(
                actual, captured_revision,
                "mutations moved the captured checkpoint behind a later I/O waiter",
            );
            assert_eq!(names[0], "current-without-another-request");
        })
        .await;
        table.server.abort();
        outcome.expect("pre-serialization mutation must not starve FIFO checkpoint progress");
    });
}

#[test]
fn a_busy_game_lease_does_not_block_another_games_durable_checkpoint() {
    run_async_test(|| async {
        let executor = GameExecutor::new(NonZeroUsize::MIN);
        let a = NativeTable::with_executor(executor.clone()).await;
        let b = NativeTable::with_executor(executor.clone()).await;
        let session = crate::lock_session(&a.app.sessions, &a.key.game_code)
            .await
            .unwrap();
        let db = a.app.game_db.clone();
        let persistence_executor = executor.clone();
        let lease_wait = executor.arm_persistence_lease();
        let (lease, ()) = executor
            .with_session(session, move |session| {
                session.advance_state_revision();
                crate::persist_full_session_async(&persistence_executor, &db, session);
            })
            .await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            lease_wait.notified().await;
            let revision = queue_named_revision(&b, "other-game-durable".to_string()).await;
            while b.app.game_db.full_saves.pending_games() != 0 {
                tokio::task::yield_now().await;
            }
            let db = b.app.game_db.clone();
            let saved =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|save| save.key == b.key)
                    .unwrap();
            assert_eq!(saved.mutation_revision, revision);
            assert_eq!(saved.persisted.display_names[0], "other-game-durable");
        })
        .await;
        // Also release the lease before reporting a negative-control timeout.
        drop(lease);
        a.server.abort();
        b.server.abort();
        outcome.expect("one game's long ordering lease held global persistence I/O admission");
    });
}

#[test]
fn serialized_checkpoints_commit_during_sustained_mutation() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const ROUNDS: usize = 3;

        let table = NativeTable::with_parallelism(NonZeroUsize::new(2).unwrap()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let executor = table.app.context.game_executor.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let (entered, mut preparations) = tokio::sync::mpsc::unbounded_channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let blocked = parking_lot::Mutex::new(blocked);
            table
                .app
                .game_db
                .set_full_save_hook(Arc::new(move |snapshot| {
                    if calls.fetch_add(1, Ordering::SeqCst) < ROUNDS {
                        entered.send(snapshot.mutation_revision).unwrap();
                        // Dropping the sender also releases this gate on test failure.
                        let _ = blocked.lock().recv();
                    }
                }));
            queue_named_revision(&table, "checkpoint-0".to_string()).await;
            let mut observations = Vec::new();
            for round in 0..ROUNDS {
                let prepared_revision = preparations.recv().await.unwrap();
                queue_named_revision(&table, format!("checkpoint-{}", round + 1)).await;
                let admission = executor.admit_persistence_io();
                tokio::pin!(admission);
                // Enqueue behind this serializer before releasing it; the next
                // checkpoint cannot overtake our durable-read observation.
                tokio::select! {
                    biased;
                    _ = &mut admission => panic!("serialization must retain I/O admission"),
                    _ = tokio::task::yield_now() => {}
                }
                release.send(()).unwrap();
                let io = admission.await;
                let db = table.app.game_db.clone();
                let loaded =
                    tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                        .await
                        .unwrap();
                let saved = loaded
                    .into_iter()
                    .find(|save| save.key == table.key)
                    .unwrap();
                observations.push((prepared_revision, saved.mutation_revision));
                drop(io);
            }
            drop(release);
            assert!(
                observations
                    .iter()
                    .all(|(prepared, saved)| saved >= prepared),
                "newer mutations starved already-prepared durable checkpoints: {observations:?}",
            );
        })
        .await;
        table.server.abort();
        outcome.expect("durability must progress without waiting for quiescence");
    });
}

#[test]
fn persistence_preflight_waits_for_io_admission_before_using_blocking_threads() {
    crate::socket_test_hooks::run_async_test_with_blocking_limit(Some(2), || async {
        let table = NativeTable::new().await;
        let io = table.app.context.game_executor.admit_persistence_io().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let db = table.app.game_db.clone();
        let held = tokio::task::spawn_blocking({
            let entered = entered.clone();
            move || db.hold_connection_for_test(entered, blocked)
        });
        entered.notified().await;
        queue_named_revision(&table, "waiting-for-admission".to_string()).await;
        tokio::task::yield_now().await;
        // One blocking worker holds SQLite's real mutex. An unadmitted
        // currency query would consume the other and strand Hello encoding.
        let hello =
            tokio::time::timeout(Duration::from_secs(2), connect_and_hello(table.url.clone()))
                .await;
        drop(release);
        held.await.unwrap();
        drop(io);
        table.server.abort();
        assert!(
            hello.is_ok(),
            "unadmitted persistence preflight starved ServerHello"
        );
    });
}

#[test]
fn removed_session_releases_its_pending_persistence_worker() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let executor = table.app.context.game_executor.clone();
        let io = executor.admit_persistence_io().await;
        queue_named_revision(&table, "removed-before-capture".to_string()).await;
        assert_eq!(table.app.game_db.full_saves.pending_games(), 1);
        table
            .app
            .sessions
            .lock()
            .await
            .remove_game(&table.key.game_code);
        drop(io);
        let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
            while table.app.game_db.full_saves.pending_games() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        table.server.abort();
        cleanup.expect("dead weak session must not strand a worker or sender entry");
    });
}

#[test]
fn retiring_old_persistence_worker_cannot_remove_its_replacement() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let executor = table.app.context.game_executor.clone();
        let db = table.app.game_db.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let held = tokio::task::spawn_blocking({
            let db = db.clone();
            let entered = entered.clone();
            move || db.hold_connection_for_test(entered, blocked)
        });
        entered.notified().await;
        queue_named_revision(&table, "cancelled-worker".to_string()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while executor.persistence_io.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old worker must enter preflight before cancellation");
        // Its real DB query cannot finish until the replacement is registered.
        // This makes stale-owner cleanup run after, not before, the cutover.
        table.app.game_db.full_saves.cancel(&table.key);
        let latest = queue_named_revision(&table, "replacement-worker".to_string()).await;
        drop(release);
        held.await.unwrap();
        let key = table.key.clone();
        let progress = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let loaded = tokio::task::spawn_blocking({
                    let db = db.clone();
                    move || db.load_active_full_sessions().unwrap()
                })
                .await
                .unwrap();
                if loaded
                    .iter()
                    .any(|save| save.key == key && save.mutation_revision == latest)
                    && db.full_saves.pending_games() == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        table.server.abort();
        progress.expect("old worker cleanup must preserve the replacement and retire on idle");
    });
}

async fn exhaust_application_quota(socket: &mut ClientSocket) {
    for _ in 0..crate::RATE_LIMIT_MESSAGES {
        send_test_message(socket, &ClientMessage::SubscribeLobby, false).await;
    }
}

async fn receive_quota_refusal(socket: &mut ClientSocket, command_id: u64) {
    loop {
        let frame = receive_command_frame(socket).await;
        if owns_command_completion(&frame, command_id) {
            assert_eq!(
                frame["type"], "ActionFailed",
                "expected quota refusal: {frame}"
            );
            return;
        }
    }
}

#[test]
fn encoded_refusal_keeps_admission_until_actual_socket_flush() {
    run_async_test(|| async {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        let table = NativeTable::new().await;
        let outcome =
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut socket = table.connect(PlayerId(0)).await;
                let count = crate::socket_mailbox::MAX_PENDING_QUOTA_REFUSALS as u64;
                exhaust_application_quota(&mut socket).await;
                let first = table.app.context.socket_hooks.arm_quota_flush();
                send_command_frame(&mut socket, &GameAction::PassPriority, 100).await;
                // The real sink has accepted the encoded frame via start_send and
                // entered poll_flush; this is not a pre-encoding mailbox barrier.
                first.entered.notified().await;
                for id in 101..100 + count {
                    send_command_frame(&mut socket, &GameAction::PassPriority, id).await;
                }
                first.release.notify_one();
                for id in 100..100 + count {
                    receive_quota_refusal(&mut socket, id).await;
                }
                // Reuse of flush-held permits is independent of the bounded
                // inspection window. Start a fresh window before exhausting again.
                tokio::time::sleep(Duration::from_secs(crate::RATE_LIMIT_WINDOW_SECS)).await;
                exhaust_application_quota(&mut socket).await;
                send_command_frame(&mut socket, &GameAction::PassPriority, 100 + count).await;
                receive_quota_refusal(&mut socket, 100 + count).await;
                assert_eq!(
                    table
                        .app
                        .context
                        .metrics
                        .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaRefusalOverflow),
                    0,
                    "a flushed budget must be reusable on the same live socket",
                );

                tokio::time::sleep(Duration::from_secs(crate::RATE_LIMIT_WINDOW_SECS)).await;
                exhaust_application_quota(&mut socket).await;
                let blocked = table.app.context.socket_hooks.arm_quota_flush();
                send_command_frame(&mut socket, &GameAction::PassPriority, 200).await;
                blocked.entered.notified().await;
                for id in 201..200 + count {
                    send_command_frame(&mut socket, &GameAction::PassPriority, id).await;
                }
                send_command_frame(&mut socket, &GameAction::PassPriority, 200 + count).await;
                let overflow =
                    tokio::time::timeout(Duration::from_millis(500), async {
                        while table.app.context.metrics.ws_disconnect_count(
                            crate::socket_disconnect::Cause::QuotaRefusalOverflow,
                        ) == 0
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await;
                // A control Close cannot preempt an in-flight RFC 6455 frame.
                // Release the flush before asserting, including negative controls.
                blocked.release.notify_one();
                overflow.expect("the encoded, unflushed refusal lost its admission permit");
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        Message::Close(Some(close)) => {
                            assert_eq!(u16::from(close.code), 1013);
                            break;
                        }
                        Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
                        frame => panic!("unexpected transport frame: {frame:?}"),
                    }
                }
            })
            .await;
        table.server.abort();
        outcome.expect("actual socket flush must own refusal admission");
    });
}

#[test]
fn legitimate_forty_click_bursts_keep_plain_and_gzip_contests_running() {
    run_async_test(|| async {
        use engine::types::contest::{ContestConfig, ContestKind, ContestPhase, ContestState};
        use engine::types::game_state::WaitingFor;
        use futures_util::SinkExt;
        use server_core::protocol::WireFormat;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let table = NativeTable::new().await;
            let work = table.app.context.socket_hooks.arm_parser_work();
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                table.keep_all().await;
                let mut socket = table.connect_with_format(PlayerId(0), format).await;
                let _opponent = table.connect(PlayerId(1)).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code).await.unwrap();
                let (lease, ()) = table.app.context.game_executor.with_session(session, |session| {
                    let config = ContestConfig {
                        countdown_ms: 1,
                        inactivity_timeout_ms: 60_000,
                        reconnect_grace_ms: 60_000,
                        max_active_ms: 60_000,
                        ..Default::default()
                    };
                    session.contest.policy.config = config;
                    session.state.phase = engine::types::phase::Phase::PreCombatMain;
                    let source_id = *session.state.objects.keys().next().unwrap();
                    session.state.waiting_for = WaitingFor::PhysicalContest {
                        contest: Box::new(ContestState::new(71, ContestKind::Staring, PlayerId(0), PlayerId(1), config)),
                        source_id,
                    };
                }).await;
                drop(lease);
                crate::ensure_contest_driver(
                    &table.app.context.game_executor, &table.app.sessions, &table.app.connections,
                    &table.app.draft_sessions, &table.app.game_db, &table.app.game_spectators,
                    &table.app.lobby, &table.app.lobby_subscribers, &table.key.game_code,
                ).await;
                loop {
                    let lease = crate::lock_session(&table.app.sessions, &table.key.game_code).await.unwrap();
                    let active = matches!(&lease.state.waiting_for, WaitingFor::PhysicalContest { contest, .. } if contest.phase == ContestPhase::Active);
                    drop(lease);
                    if active { break; }
                    tokio::task::yield_now().await;
                }
                for seq in 1..=40 {
                    send_test_message(&mut socket, &ClientMessage::ContestClick { contest: 71, seq }, format.is_some()).await;
                }
                // Hello + Reconnect spent two of thirty slots. The frozen quota
                // clock proves all forty clicks arrive in the same work window.
                work.wait_rate_limited(12).await;
                let mut command = serde_json::to_value(ClientMessage::Action { action: GameAction::PassPriority }).unwrap();
                command["command_id"] = serde_json::json!(719_001);
                command["padding"] = serde_json::json!("x".repeat(4096));
                let json = command.to_string();
                assert!(json.len() > 1024, "exercise a command beyond the old compact recovery cap");
                let frame = match crate::wire::encode_json_message_sync(json, format.is_some()).unwrap() {
                    axum::extract::ws::Message::Text(text) => WsMessage::Text(text.to_string().into()),
                    axum::extract::ws::Message::Binary(bytes) => WsMessage::Binary(bytes),
                    _ => unreachable!("an application command is text or binary"),
                };
                socket.send(frame).await.unwrap();
                work.wait_rate_limited(13).await;
                send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 719 }, format.is_some()).await;
                let mut pong = false;
                let mut progress = false;
                let mut refusal = false;
                while !pong || !progress || !refusal {
                    let json = receive_wire_json(&mut socket).await;
                    match serde_json::from_str::<ServerMessage>(&json).unwrap() {
                        ServerMessage::Pong { timestamp: 719 } => pong = true,
                        ServerMessage::ActionFailed { .. } => {
                            let header: serde_json::Value = serde_json::from_str(&json).unwrap();
                            assert_eq!(header["command_id"], 719_001);
                            refusal = true;
                        }
                        ServerMessage::ContestProgress { contest: 71, clicks, .. } => {
                            for (player, count) in clicks {
                                if player == PlayerId(0) {
                                    assert!(count <= 40, "a click was applied twice");
                                    progress |= count == 40;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                assert_eq!(work.counts(), (crate::RATE_LIMIT_MESSAGES as usize + 12, 13),
                    "only twelve recovery-admitted compact clicks get an extra typed parse");
                assert_eq!(table.app.context.metrics.ws_quota_discard_count(), 0);
            }).await;
            table.server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{format:?} forty-click burst stalled or closed its socket: {error}")
            });
        }
    });
}

#[test]
fn policy_compliant_click_refill_preserves_correlated_completion() {
    run_async_test(|| async {
        use engine::types::contest::{ContestConfig, ContestKind, ContestPhase, ContestState};
        use engine::types::game_state::WaitingFor;
        use futures_util::{SinkExt, StreamExt};
        use server_core::protocol::WireFormat;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let table = NativeTable::new().await;
            let work = table.app.context.socket_hooks.arm_parser_work();
            let outcome = tokio::time::timeout(Duration::from_secs(20), async {
                table.keep_all().await;
                let mut socket = table.connect_with_format(PlayerId(0), format).await;
                let _opponent = table.connect(PlayerId(1)).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let (lease, burst) = table
                    .app
                    .context
                    .game_executor
                    .with_session(session, |session| {
                        let config = ContestConfig {
                            countdown_ms: 0,
                            inactivity_timeout_ms: 60_000,
                            reconnect_grace_ms: 60_000,
                            max_active_ms: 60_000,
                            ..Default::default()
                        };
                        session.contest.policy.config = config;
                        session.state.phase = engine::types::phase::Phase::PreCombatMain;
                        let source_id = *session.state.objects.keys().next().unwrap();
                        session.state.waiting_for = WaitingFor::PhysicalContest {
                            contest: Box::new(ContestState::new(
                                72,
                                ContestKind::Staring,
                                PlayerId(0),
                                PlayerId(1),
                                config,
                            )),
                            source_id,
                        };
                        session.contest.policy.click_burst
                    })
                    .await;
                drop(lease);
                let clicks = burst + 20;
                assert!(clicks >= 60, "exercise the old exhausted-refusal boundary");
                crate::ensure_contest_driver(
                    &table.app.context.game_executor,
                    &table.app.sessions,
                    &table.app.connections,
                    &table.app.draft_sessions,
                    &table.app.game_db,
                    &table.app.game_spectators,
                    &table.app.lobby,
                    &table.app.lobby_subscribers,
                    &table.key.game_code,
                )
                .await;
                loop {
                    let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let active = matches!(&lease.state.waiting_for,
                        WaitingFor::PhysicalContest { contest, .. }
                            if contest.phase == ContestPhase::Active);
                    drop(lease);
                    if active {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                for seq in 1..=clicks {
                    if seq > burst {
                        // Twenty refill clicks over ~700 ms stay below the
                        // policy's 30/s rate after its initial forty-click burst.
                        tokio::time::sleep(Duration::from_millis(35)).await;
                    }
                    send_test_message(
                        &mut socket,
                        &ClientMessage::ContestClick { contest: 72, seq },
                        format.is_some(),
                    )
                    .await;
                    // One frozen decoder window, without letting ingress
                    // capacity conceal the distinct work-allowance regression.
                    work.wait_completed(2 + seq as usize).await;
                }
                let mut command = serde_json::to_value(ClientMessage::Action {
                    action: GameAction::PassPriority,
                })
                .unwrap();
                command["command_id"] = serde_json::json!(720_001);
                command["padding"] = serde_json::json!("x".repeat(4096));
                let frame =
                    crate::wire::encode_json_message_sync(command.to_string(), format.is_some())
                        .unwrap();
                let frame = match frame {
                    axum::extract::ws::Message::Text(text) => {
                        WsMessage::Text(text.to_string().into())
                    }
                    axum::extract::ws::Message::Binary(bytes) => WsMessage::Binary(bytes),
                    _ => unreachable!("application command encoding"),
                };
                socket.send(frame).await.unwrap();
                work.wait_completed(3 + clicks as usize).await;
                loop {
                    let json = receive_wire_json(&mut socket).await;
                    if matches!(
                        serde_json::from_str::<ServerMessage>(&json).unwrap(),
                        ServerMessage::ActionFailed { .. }
                    ) {
                        let header: serde_json::Value = serde_json::from_str(&json).unwrap();
                        assert_eq!(header["command_id"], 720_001);
                        break;
                    }
                }
                // The frozen probe window may be spent by legitimate clicks;
                // native control liveness does not depend on JSON probe credit.
                socket
                    .send(WsMessage::Ping(vec![72, 1].into()))
                    .await
                    .unwrap();
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        WsMessage::Pong(bytes) if bytes.as_ref() == [72, 1] => break,
                        WsMessage::Text(_)
                        | WsMessage::Binary(_)
                        | WsMessage::Ping(_)
                        | WsMessage::Pong(_) => {}
                        frame => panic!("legitimate policy traffic closed the socket: {frame:?}"),
                    }
                }
                let quota = crate::RATE_LIMIT_MESSAGES as usize;
                let over_quota = clicks as usize - (quota - 2);
                assert_eq!(work.counts(), (quota * 2, over_quota + 1));
                assert_eq!(
                    table.app.context.metrics.ws_quota_discard_count(),
                    (over_quota - quota) as u64,
                );
                assert_eq!(
                    table
                        .app
                        .context
                        .metrics
                        .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaDiscardOverflow),
                    0,
                );
            })
            .await;
            table.server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{format:?} maximal contest stream lost owned completion: {error}")
            });
        }
    });
}

async fn age_hosted_listing(
    app: &crate::AppState,
    code: &str,
) -> Vec<lobby_broker::LobbyRegistration> {
    let mut broker = app.lobby.lock().await;
    // Preserve the registration generation while advancing its fixture age.
    // No production clock override or five-minute wall-clock sleep is needed.
    let mut snapshot = serde_json::to_value(broker.lobby()).unwrap();
    let game = &mut snapshot["games"][code];
    game["created_at"] = serde_json::json!(0);
    game["last_seen"] = serde_json::json!(0);
    *broker.lobby_mut() = serde_json::from_value(snapshot).unwrap();
    let reported = broker.lobby().check_expired(300, &crate::SysEnv);
    assert!(reported
        .iter()
        .any(|registration| registration.game_code() == code));
    reported
}

#[test]
fn socket_hosted_json_pings_keep_the_listing_alive() {
    run_async_test(|| async {
        for mode in [crate::ServerMode::Full, crate::ServerMode::LobbyOnly] {
            let (url, server, _directory, app) = spawn_server(
                mode,
                crate::draft_pools::DraftPools::default(),
                Arc::new(basic_land_test_database()),
                crate::ServerContext::default(),
            )
            .await;
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                let mut socket = connect_and_hello(url).await;
                send_test_message(
                    &mut socket,
                    &ClientMessage::CreateGameWithSettings {
                        deck: DeckData {
                            main_deck: vec!["Network Test Land".to_string(); 60],
                            ..Default::default()
                        },
                        display_name: "Heartbeat Host".to_string(),
                        public: true,
                        password: None,
                        timer_seconds: None,
                        player_count: 2,
                        match_config: Default::default(),
                        ai_seats: Vec::new(),
                        format_config: Some(FormatConfig::free_for_all()),
                        room_name: None,
                        host_peer_id: matches!(mode, crate::ServerMode::LobbyOnly)
                            .then(|| "heartbeat-peer".to_string()),
                        draft_metadata: None,
                        start_when_full: true,
                        ranked: false,
                        requested_code: None,
                        booster_pack_pool: None,
                    },
                    false,
                )
                .await;
                let code = loop {
                    match recv_server_message(&mut socket).await {
                        ServerMessage::GameCreated { game_code, .. } => break game_code,
                        ServerMessage::Error { message, .. } => {
                            panic!("{mode:?} host creation failed: {message}")
                        }
                        _ => {}
                    }
                };
                // Delimit creation through the application FIFO before aging.
                // Only broker-hosted rooms publish a heartbeat registration.
                send_test_message(&mut socket, &ClientMessage::SubscribeLobby, false).await;
                loop {
                    if matches!(
                        recv_server_message(&mut socket).await,
                        ServerMessage::LobbyUpdate { .. }
                    ) {
                        break;
                    }
                }
                for timestamp in 1..=3 {
                    if matches!(mode, crate::ServerMode::LobbyOnly) {
                        let _ = age_hosted_listing(&app, &code).await;
                    }
                    send_test_message(&mut socket, &ClientMessage::Ping { timestamp }, false).await;
                    loop {
                        if matches!(
                            recv_server_message(&mut socket).await,
                            ServerMessage::Pong { timestamp: echoed } if echoed == timestamp
                        ) {
                            break;
                        }
                    }
                    match mode {
                        crate::ServerMode::Full => {
                            // Full rooms are session-owned, not broker-owned.
                            // Force a stale report AFTER the Pong, then drive
                            // the same session/draft and lobby phases as main's
                            // production expiry task. A live host must keep its
                            // room even without a broker heartbeat stamp.
                            let mut reported = age_hosted_listing(&app, &code).await;
                            tokio::time::timeout(Duration::from_secs(5), async {
                                loop {
                                    let drafts =
                                        crate::live_draft_codes(&app.draft_sessions, &reported)
                                            .await;
                                    let sweep = {
                                        let mut sessions = app.sessions.lock().await;
                                        crate::handle_expired_lobby_games(
                                            &mut sessions,
                                            &app.game_db,
                                            &reported,
                                            &drafts,
                                        )
                                    };
                                    let routes = crate::ExpiredLobbyRoutes::capture(
                                        &sweep,
                                        &app.connections,
                                        &app.game_spectators,
                                    )
                                    .await;
                                    crate::finish_lobby_expiry_sweep(
                                        &app.lobby,
                                        &app.lobby_subscribers,
                                        &app.connections,
                                        &app.game_spectators,
                                        &sweep,
                                        routes,
                                    )
                                    .await;
                                    assert!(
                                        app.sessions.lock().await.contains_game(&code),
                                        "lobby age retired a connected Full host's room",
                                    );
                                    let broker = app.lobby.lock().await;
                                    assert!(broker.lobby().public_game(&code).is_some());
                                    reported = broker.lobby().check_expired(300, &crate::SysEnv);
                                    if !reported
                                        .iter()
                                        .any(|registration| registration.game_code() == code)
                                    {
                                        break;
                                    }
                                    drop(broker);
                                    // A busy session is legitimately deferred
                                    // by the sweep; model its next tick.
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("the production sweep failed to keep a live Full room");
                        }
                        crate::ServerMode::LobbyOnly => {
                            // Pong and HostRefresh are independent. Do not send
                            // another broker command that could mask a missing
                            // refresh from this actual socket heartbeat.
                            tokio::time::timeout(Duration::from_secs(5), async {
                                loop {
                                    let broker = app.lobby.lock().await;
                                    assert!(broker.lobby().public_game(&code).is_some());
                                    if !broker
                                        .lobby()
                                        .check_expired(300, &crate::SysEnv)
                                        .iter()
                                        .any(|registration| registration.game_code() == code)
                                    {
                                        break;
                                    }
                                    drop(broker);
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("a real JSON Ping failed to refresh its broker-hosted listing");
                        }
                    }
                }
            })
            .await;
            server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{mode:?} hosted socket lifecycle failed: {error}");
            });
        }
    });
}

fn codec_deck() -> DeckData {
    DeckData {
        main_deck: vec!["Network Test Land".to_string(); 60],
        ..Default::default()
    }
}

async fn codec_create(table: &NativeTable, players: u8) -> (FullSessionKey, CodecPeer) {
    let mut host = codec_hello(&table.url, false, Some(serde_json::json!(["StateCodecV1"]))).await;
    host.send(&crate::socket_test_hooks::room_request(
        codec_deck(),
        players,
        None,
    ))
    .await;
    loop {
        let reply = host.next().await;
        match reply.value["type"].as_str().unwrap() {
            "GameCreated" => {
                return (
                    serde_json::from_value(reply.value["data"]["full_key"].clone()).unwrap(),
                    host,
                )
            }
            "Error" => panic!("create failed: {}", reply.value),
            _ => {}
        }
    }
}

#[test]
fn codec_dual_attachment_cross_game_authority() {
    run_async_test(|| async {
        use crate::socket_test_hooks::ResyncPoint;
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let mut peer = codec_seat(&table, PlayerId(0), false).await;
            let (other_key, mut host) = codec_create(&table, 2).await;
            let mut guest =
                codec_hello(&table.url, false, Some(serde_json::json!(["StateCodecV1"]))).await;
            guest
                .send(&ClientMessage::JoinGame {
                    game_code: other_key.game_code.clone(),
                    deck: codec_deck(),
                })
                .await;
            assert_projection(&guest.state().await.value, &other_key, PlayerId(1));
            assert_projection(&host.state().await.value, &other_key, PlayerId(0));
            assert_ne!(other_key, table.key);
            peer.send(&ClientMessage::SpectatorJoin {
                game_code: other_key.game_code.clone(),
            })
            .await;
            assert_projection(
                &peer.state().await.value,
                &other_key,
                crate::SPECTATOR_PLAYER_ID,
            );
            assert_projection(&peer.resync().await.value, &table.key, PlayerId(0));
            let built = table
                .app
                .context
                .socket_hooks
                .arm_resync(ResyncPoint::Built);
            peer.send(&ClientMessage::ResyncState).await;
            built.entered.notified().await;
            table
                .app
                .connections
                .lock()
                .await
                .get_mut(&table.key.game_code)
                .unwrap()
                .remove(&PlayerId(0));
            built.release.notify_one();
            let public = peer.state().await;
            assert_eq!(public.kind, Some(state_codec::FrameKind::Full));
            assert_projection(&public.value, &other_key, crate::SPECTATOR_PLAYER_ID);
            let session = crate::lock_session(&table.app.sessions, &other_key.game_code)
                .await
                .unwrap();
            let reference =
                serde_json::to_value(crate::build_resync_state_message(&session, None).unwrap())
                    .unwrap();
            assert_eq!(
                public.value, reference,
                "different-game spectator fallback is structural parity"
            );
            drop(session);
            peer.baseline = None;
            assert_projection(
                &peer.resync().await.value,
                &other_key,
                crate::SPECTATOR_PLAYER_ID,
            );
        })
        .await;
        table.server.abort();
        outcome.expect("sequential cross-game leases and selected attachment authority");
    });
}

#[test]
fn codec_prestart_and_builder_provenance() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            let (key, mut host) = codec_create(&table, 4).await;
            let mut joiner =
                codec_hello(&table.url, false, Some(serde_json::json!(["StateCodecV1"]))).await;
            joiner
                .send(&ClientMessage::JoinGame {
                    game_code: key.game_code.clone(),
                    deck: codec_deck(),
                })
                .await;
            let prestart = joiner.state().await;
            assert_eq!(prestart.value["type"], "StateUpdate");
            assert_eq!(prestart.kind, Some(state_codec::FrameKind::Full));
            assert_projection(&prestart.value, &key, PlayerId(1));
            let recovered = joiner.resync().await;
            assert_projection(&recovered.value, &key, PlayerId(1));
            assert_eq!(
                recovered.value["data"]["legal_actions"],
                serde_json::json!([])
            );
            host.resync().await;
            // Draft attachment, direct start, broadcast and reconnect all consume
            // this same session-bound builder; their broader lifecycle is phase 3.
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let decision = crate::DecisionSnapshot::from_state(&session.state);
            for player in [PlayerId(0), PlayerId(1), PlayerId(3)] {
                let message = crate::build_game_started_message(
                    &session,
                    &decision,
                    player,
                    None,
                    Vec::new(),
                );
                assert_projection(&serde_json::to_value(message).unwrap(), &table.key, player);
            }
            let spectator = crate::build_spectator_game_started_message(&session).unwrap();
            assert_projection(
                &serde_json::to_value(spectator).unwrap(),
                &table.key,
                crate::SPECTATOR_PLAYER_ID,
            );
        })
        .await;
        table.server.abort();
        outcome.expect("all native producer builders carry runtime/viewer provenance");
    });
}
