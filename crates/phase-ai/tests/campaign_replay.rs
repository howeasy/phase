//! Deterministic, local campaign contract tests. No public feeds or model calls.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use engine::ai_support::AiDecisionContract;
use engine::database::CardDatabase;
use engine::game::deck_loading::{resolve_deck_list, DeckList, PlayerDeckList};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use phase_ai::auto_play::{
    run_driver_loop, run_driver_loop_observed, AiActionResult, AiActionsStop, DriverExit,
    DriverHooks,
};
use phase_ai::campaign::{self, CampaignJournal, CampaignManifest, Observation, ObservationDelta};
use phase_ai::config::{create_config_for_players, AiConfig, AiDifficulty, Platform};
use phase_ai::session::AiSession;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    manifest: CampaignManifest,
    db: Arc<CardDatabase>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "campaign-replay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        campaign::write_smoke_inputs(&root).expect("create real fixture inputs");
        let manifest =
            CampaignManifest::parse(&std::fs::read(root.join("capped.json")).unwrap()).unwrap();
        let db = Arc::new(CardDatabase::from_export(&root.join("card-data.json")).unwrap());
        Self { root, manifest, db }
    }
    fn state(&self) -> GameState {
        let seat = PlayerDeckList {
            main_deck: vec!["Campaign Fixture Land".into(); 12],
            commander: vec!["Campaign Fixture Commander".into()],
            ..Default::default()
        };
        let decks = DeckList {
            player: seat.clone(),
            opponent: seat.clone(),
            ai_decks: vec![seat.clone(), seat],
            ..Default::default()
        };
        let mut state =
            campaign::build_game_state(&self.db, &resolve_deck_list(&self.db, &decks), 4, 42);
        engine::game::engine::start_game(&mut state);
        state
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn rekey(manifest: &mut CampaignManifest) {
    manifest.game_key = campaign::canonical_hash(&manifest.identity()).unwrap();
}

#[test]
fn exported_config_templates_admit_seeded_cases_but_not_arbitrary_parameters() {
    let fixture = Fixture::new();
    let catalog = campaign::config_catalog().unwrap();
    let pointer = catalog["measurement_seed_pointer"].as_str().unwrap();
    for row in catalog["anchors"]
        .as_array()
        .unwrap()
        .iter()
        .chain(catalog["candidates"].as_array().unwrap())
    {
        for seed in [0, 42, u64::MAX] {
            let mut effective = row["effective_config"].clone();
            *effective.pointer_mut(pointer).unwrap() = json!(seed);
            let mut manifest = fixture.manifest.clone();
            manifest.seed = seed.to_string();
            manifest.information_model = campaign::InformationModel::OpenDecklistFairV1;
            manifest.fair_contract_version = 1;
            manifest.sampling_seed = Some("7919".into());
            manifest.seats = std::array::from_fn(|_| effective.clone());
            rekey(&mut manifest);
            manifest
                .validate()
                .expect("native catalog must admit its seeded template");
            manifest.seats[0]["profile"]["risk_tolerance"] = json!(0.123);
            rekey(&mut manifest);
            assert!(
                manifest.validate().is_err(),
                "catalog is not an arbitrary knob editor"
            );
        }
    }
}
fn configs() -> HashMap<PlayerId, AiConfig> {
    (0..4)
        .map(|seat| {
            (
                PlayerId(seat),
                create_config_for_players(AiDifficulty::Easy, Platform::Native, 4)
                    .into_measurement(42),
            )
        })
        .collect()
}
fn records(root: &Path) -> Vec<Value> {
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ndjson"))
        .collect();
    paths.sort();
    paths
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn manifest_requires_explicit_nulls_and_rejects_unknown_duplicate_fields() {
    let f = Fixture::new();
    for field in [
        "sampling_seed",
        "phase",
        "panel_id",
        "block_id",
        "pair_id",
        "role",
        "anchor_tier",
        "evaluated_seat",
    ] {
        let mut value = serde_json::to_value(&f.manifest).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            CampaignManifest::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
            "missing {field}"
        );
    }
    let mut value = serde_json::to_value(&f.manifest).unwrap();
    value["unexpected"] = json!(true);
    assert!(CampaignManifest::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    let text = serde_json::to_string(&f.manifest).unwrap();
    let duplicate = text.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(CampaignManifest::parse(duplicate.as_bytes()).is_err());
    let duplicate = text.replacen(
        "\"difficulty\":\"Easy\"",
        "\"difficulty\":\"Easy\",\"difficulty\":\"Easy\"",
        1,
    );
    assert!(CampaignManifest::parse(duplicate.as_bytes()).is_err());
}

#[test]
fn strict_seed_cap_four_seats_and_effective_config_contracts() {
    let f = Fixture::new();
    for invalid in ["", "00", "-1", "1.0", "18446744073709551616"] {
        assert!(campaign::decimal_seed(invalid).is_err());
    }
    assert_eq!(
        campaign::decimal_seed("18446744073709551615").unwrap(),
        u64::MAX
    );
    for cap in [0, 2, 199999, 200001] {
        let mut manifest = f.manifest.clone();
        manifest.action_cap = cap;
        rekey(&mut manifest);
        assert!(manifest.validate().is_err());
    }
    let mut manifest = f.manifest.clone();
    manifest.track = "exploration".into();
    rekey(&mut manifest);
    assert!(manifest.validate().is_err());
    manifest.action_cap = 200000;
    rekey(&mut manifest);
    assert!(manifest.validate().is_ok());
    manifest.sampling_seed = Some("4".into());
    rekey(&mut manifest);
    assert!(manifest.validate().is_err());
    let mut value = serde_json::to_value(&f.manifest).unwrap();
    value["seats"].as_array_mut().unwrap().pop();
    assert!(CampaignManifest::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut manifest = f.manifest.clone();
    manifest.seats[0]["search"]["max_nodes"] = json!(999);
    rekey(&mut manifest);
    assert!(manifest.validate().is_err());
    let mut manifest = f.manifest.clone();
    manifest.seats[0]["execution_mode"] = json!("Interactive");
    rekey(&mut manifest);
    assert!(manifest.validate().is_err());
}

#[test]
fn canonical_json_matches_python_depot_number_spelling() {
    let fixture = json!({"unicode":"Éomer","z":1.0,"integer":1,"negative_zero":-0.0,"tiny":0.00001,"large":1e20,"plain":0.0001});
    assert_eq!(String::from_utf8(campaign::canonical_bytes(&fixture).unwrap()).unwrap(),
        "{\"integer\":1,\"large\":1e+20,\"negative_zero\":-0.0,\"plain\":0.0001,\"tiny\":1e-05,\"unicode\":\"Éomer\",\"z\":1.0}");
    assert_ne!(
        campaign::canonical_hash(&json!(1)).unwrap(),
        campaign::canonical_hash(&json!(1.0)).unwrap()
    );
}

#[test]
fn membership_metadata_is_not_behavior_identity() {
    let f = Fixture::new();
    let mut other = f.manifest.clone();
    other.phase = Some("screen".into());
    other.panel_id = Some(8);
    other.pair_id = Some("pair".into());
    assert_eq!(
        campaign::canonical_hash(&f.manifest.identity()).unwrap(),
        campaign::canonical_hash(&other.identity()).unwrap()
    );
    other.seed = "43".into();
    assert_ne!(
        campaign::canonical_hash(&f.manifest.identity()).unwrap(),
        campaign::canonical_hash(&other.identity()).unwrap()
    );
}

#[test]
fn actual_source_preset_risks_are_pinned_without_rewriting() {
    let f = Fixture::new();
    for difficulty in [AiDifficulty::Medium, AiDifficulty::Hard, AiDifficulty::CEDH] {
        let config =
            create_config_for_players(difficulty, Platform::Native, 4).into_measurement(42);
        let mut manifest = f.manifest.clone();
        manifest.information_model = campaign::InformationModel::OpenDecklistFairV1;
        manifest.fair_contract_version = 1;
        manifest.sampling_seed = Some("77".into());
        manifest.seats = std::array::from_fn(|_| serde_json::to_value(&config).unwrap());
        rekey(&mut manifest);
        let actual = manifest.configs().unwrap();
        assert_eq!(
            actual[&PlayerId(0)].profile.risk_tolerance,
            config.profile.risk_tolerance
        );
        assert!(manifest.validate().is_ok());
    }
}

#[test]
fn observation_delta_replaces_every_changed_field_and_removes_objects() {
    let f = Fixture::new();
    let state = f.state();
    let before = Observation::capture(&state).unwrap();
    assert!(before
        .objects
        .values()
        .any(|object| object["name"] == "Campaign Fixture Land"));
    let mut after = before.clone();
    let id = after.objects.keys().next().unwrap().clone();
    after.objects.get_mut(&id).unwrap()["name"] = json!("changed characteristics");
    after.objects.get_mut(&id).unwrap()["attachments"] = json!([991]);
    after.objects.get_mut(&id).unwrap()["counters"] = json!({"PlusOnePlusOne":3});
    let removed = after
        .objects
        .keys()
        .find(|key| **key != id)
        .unwrap()
        .clone();
    after.objects.remove(&removed);
    after.players.get_mut("0").unwrap()["life"] = json!(17);
    after.zones.insert("0:library".into(), json!([41, 7, 99]));
    after
        .stack
        .insert("0".into(), json!({"id":991,"controller":2}));
    after.context["waiting_for"] = json!({"type":"GameOver","winner":0});
    let delta = ObservationDelta::between(&before, &after);
    assert!(delta.objects.upserts.contains_key(&id));
    assert!(delta.objects.removals.contains(&removed));
    let mut reconstructed = before;
    delta.apply(&mut reconstructed);
    assert_eq!(reconstructed, after);
}

#[derive(Default)]
struct TraceHooks {
    actions: Vec<GameAction>,
    hashes: Vec<String>,
    stop_after: Option<usize>,
    illegal: bool,
    host_events: usize,
}
impl DriverHooks for TraceHooks {
    fn choose(
        &mut self,
        state: &GameState,
        config: &AiConfig,
        contract: &AiDecisionContract,
        rng: &mut impl Rng,
        session: Option<&Arc<AiSession>>,
    ) -> Result<Option<GameAction>, String> {
        if self.illegal {
            return Ok(Some(GameAction::PassPriority));
        }
        Ok(phase_ai::choose_action_with_issued_contract(
            state,
            config,
            rng,
            session.expect("omniscient test session"),
            contract,
        ))
    }
    fn applied(
        &mut self,
        _actor: PlayerId,
        _owner: PlayerId,
        _pass: bool,
        result: &AiActionResult,
    ) -> Result<(), String> {
        self.actions.push(result.action.clone());
        self.hashes
            .push(campaign::deterministic_state_hash(&result.state)?);
        if self.stop_after == Some(self.actions.len()) {
            return Err("observer refused next transition".into());
        }
        Ok(())
    }
    fn host_settled(&mut self, _state: &GameState, events: &[GameEvent]) -> Result<(), String> {
        self.host_events += events.len();
        Ok(())
    }
}

#[test]
fn observed_and_ordinary_drivers_choose_the_same_actions_and_states() {
    let f = Fixture::new();
    let mut ordinary = f.state();
    let mut observed = ordinary.clone();
    let players = (0..4).map(PlayerId).collect::<HashSet<_>>();
    let configs = configs();
    let mut actions = Vec::new();
    let mut hashes = Vec::new();
    let session = AiSession::arc_from_game(&ordinary);
    run_driver_loop(
        &mut ordinary,
        &players,
        &configs,
        &mut rand::rngs::StdRng::seed_from_u64(42),
        &session,
        12,
        &mut |run, _, _| {
            for result in &run.results {
                actions.push(result.action.clone());
                hashes.push(campaign::deterministic_state_hash(&result.state).unwrap());
            }
        },
    );
    let mut hooks = TraceHooks::default();
    let session = AiSession::arc_from_game(&observed);
    run_driver_loop_observed(
        &mut observed,
        &players,
        &configs,
        &mut rand::rngs::StdRng::seed_from_u64(42),
        Some(&session),
        12,
        &mut hooks,
    )
    .unwrap();
    assert_eq!(hooks.actions, actions);
    assert_eq!(hooks.hashes, hashes);
    assert_eq!(
        campaign::deterministic_state_hash(&ordinary).unwrap(),
        campaign::deterministic_state_hash(&observed).unwrap()
    );
}

#[test]
fn hook_failure_stops_before_another_action_and_real_contract_rejects_illegal_choice() {
    let f = Fixture::new();
    let mut state = f.state();
    let players = (0..4).map(PlayerId).collect();
    let mut hooks = TraceHooks {
        stop_after: Some(2),
        ..Default::default()
    };
    let session = AiSession::arc_from_game(&state);
    assert!(run_driver_loop_observed(
        &mut state,
        &players,
        &configs(),
        &mut rand::rngs::StdRng::seed_from_u64(42),
        Some(&session),
        50,
        &mut hooks
    )
    .is_err());
    assert_eq!(hooks.actions.len(), 2);
    let mut state = f.state();
    let before = campaign::deterministic_state_hash(&state).unwrap();
    let mut hooks = TraceHooks {
        illegal: true,
        ..Default::default()
    };
    let session = AiSession::arc_from_game(&state);
    let outcome = run_driver_loop_observed(
        &mut state,
        &players,
        &configs(),
        &mut rand::rngs::StdRng::seed_from_u64(42),
        Some(&session),
        50,
        &mut hooks,
    )
    .unwrap();
    assert!(matches!(
        outcome.exit,
        DriverExit::BatchBreak(AiActionsStop::ApplyFailed { .. })
    ));
    assert!(hooks.actions.is_empty());
    assert_eq!(campaign::deterministic_state_hash(&state).unwrap(), before);
}

#[test]
fn journal_seals_hash_bound_segments_and_never_skips_sequence() {
    let f = Fixture::new();
    let output = f.root.join("segments");
    let mut writer = CampaignJournal::create(&f.manifest, &output).unwrap();
    writer
        .record("initial", json!({"checkpoint":{}}), true)
        .unwrap();
    for ordinal in 0..129 {
        writer
            .record("action", json!({"ordinal":ordinal}), ordinal == 127)
            .unwrap();
    }
    writer
        .record("terminal", json!({"outcome":"capped"}), false)
        .unwrap();
    let rows = records(&output);
    for (seq, row) in rows.iter().skip(1).enumerate() {
        assert_eq!(row["seq"], seq);
    }
    let indexes: Vec<_> = std::fs::read_dir(&output)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("index-")
        })
        .collect();
    assert_eq!(indexes.len(), 2);
    let index: Value =
        serde_json::from_slice(&std::fs::read(output.join("index-000002.json")).unwrap()).unwrap();
    for segment in index["segments"].as_array().unwrap() {
        let bytes = std::fs::read(output.join(segment["path"].as_str().unwrap())).unwrap();
        assert_eq!(segment["sha256"], format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(segment["bytes"], bytes.len());
    }
}

#[test]
fn capped_native_fixture_has_typed_actions_exact_decisions_and_reducible_state() {
    let f = Fixture::new();
    let output = f.root.join("capped-attempt");
    let terminal = campaign::run_manifest(&f.root.join("capped.json"), &output).unwrap();
    assert_eq!(terminal["outcome"], "capped");
    assert_eq!(terminal["actions"], 1);
    assert_eq!(terminal["decisions"], 1);
    assert!(
        terminal["recorder_ns"].as_u64().unwrap() <= terminal["duration_ns"].as_u64().unwrap(),
        "recorder intervals must be a subset of the same game timing interval",
    );
    assert_eq!(
        terminal["timing_scope"],
        "recorder_initialization_through_terminal_payload",
    );
    assert_eq!(terminal["terminal_encoding_included"], false);
    for seat in terminal["per_seat"].as_array().unwrap() {
        let zones = seat["zones"].as_object().unwrap();
        assert_eq!(zones["Command"], 1);
        assert_eq!(zones["Hand"], seat["hand"]);
        assert_eq!(zones["Library"], seat["library"]);
        assert!(zones.keys().all(|zone| !zone.contains('"')));
    }
    let rows = records(&output);
    let initial = rows.iter().find(|row| row["type"] == "initial").unwrap();
    let mut observation: Observation =
        serde_json::from_value(initial["payload"]["checkpoint"].clone()).unwrap();
    let action = rows.iter().find(|row| row["type"] == "action").unwrap();
    let _: engine::types::replay::RecordedAction =
        serde_json::from_value(action["payload"]["recorded_action"].clone()).unwrap();
    let delta: ObservationDelta =
        serde_json::from_value(action["payload"]["spectator"]["delta"].clone()).unwrap();
    delta.apply(&mut observation);
    assert_eq!(
        campaign::canonical_hash(&serde_json::to_value(&observation).unwrap()).unwrap(),
        action["payload"]["observation_hash"]
    );
    let decision = &rows.iter().find(|row| row["type"] == "decision").unwrap()["payload"];
    assert_eq!(
        decision["latency_ns"].as_u64().unwrap(),
        decision["materialize_ns"].as_u64().unwrap() + decision["chooser_ns"].as_u64().unwrap()
    );
    let second = f.root.join("second-attempt");
    campaign::run_manifest(&f.root.join("capped.json"), &second).unwrap();
    let repeated = records(&second);
    let repeated_action = repeated.iter().find(|r| r["type"] == "action").unwrap();
    assert_eq!(
        action["payload"]["deterministic_hash"],
        repeated_action["payload"]["deterministic_hash"]
    );
}

#[test]
fn stage_paths_are_confined_and_existing_outputs_are_never_overwritten() {
    let f = Fixture::new();
    let mut manifest = f.manifest.clone();
    manifest.export_path = "../card-data.json".into();
    let path = f.root.join("escape.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(campaign::run_manifest(&path, &f.root.join("escape-output")).is_err());
    assert!(CampaignJournal::create(&f.manifest, &f.root).is_err());
}

#[test]
fn native_normalization_is_permutation_invariant_and_reports_name_fallbacks() {
    let f = Fixture::new();
    let names = vec![
        "Campaign Fixture Land".into(),
        "Campaign Fixture Commander".into(),
    ];
    let mut reversed = names.clone();
    reversed.reverse();
    assert_eq!(
        campaign::canonical_deck_section(&f.db, &names).unwrap(),
        campaign::canonical_deck_section(&f.db, &reversed).unwrap()
    );
    assert_eq!(
        campaign::normalized_deck_id(&f.db, &names, &[], &[]).unwrap(),
        campaign::normalized_deck_id(&f.db, &reversed, &[], &[]).unwrap()
    );
    let requests = json!({"schema_version":1,"decks":[{"main_deck":vec!["Campaign Fixture Land";99],"commander":["Campaign Fixture Commander"],"companion":[]}]});
    let input = f.root.join("normalize-requests.json");
    std::fs::write(&input, serde_json::to_vec(&requests).unwrap()).unwrap();
    let output = f.root.join("normalized.json");
    campaign::normalize_corpus(&f.root.join("card-data.json"), &input, &output).unwrap();
    let normalized: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(normalized["identity_scheme_version"], 1);
    assert_eq!(
        normalized["identity_fallbacks"].as_array().unwrap().len(),
        2
    );
    assert_eq!(normalized["decks"][0]["deck_id"], f.manifest.pod[0]);
}

#[test]
fn capabilities_are_exclusive_and_require_no_card_database() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ai-commander"))
        .arg("--campaign-capabilities")
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(output.status.success());
    let capabilities: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(capabilities["protocol_version"], 1);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ai-commander"))
        .args(["--campaign-capabilities", "--seed", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn real_in_memory_game_completes_and_every_action_reduces_to_the_last_checkpoint() {
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            let f = Fixture::new();
            // A deliberately small in-memory rules fixture completes by the engine's
            // real library exhaustion rules. Production loading still requires 100.
            let seat = PlayerDeckList {
                main_deck: vec!["Campaign Fixture Land".into(); 8],
                commander: vec!["Campaign Fixture Commander".into()],
                ..Default::default()
            };
            let list = DeckList {
                player: seat.clone(),
                opponent: seat.clone(),
                ai_decks: vec![seat.clone(), seat.clone()],
                ..Default::default()
            };
            let mut state =
                campaign::build_game_state(&f.db, &resolve_deck_list(&f.db, &list), 4, 42);
            let initial = engine::game::engine::start_game(&mut state);
            let header = engine::types::replay::ReplayHeader {
                format_config: engine::types::format::FormatConfig::commander(),
                match_config: Default::default(),
                player_count: 4,
                first_player: None,
                seed: 42,
                deck_data: Some(list),
            };
            let mut manifest = f.manifest.clone();
            manifest.action_cap = 200000;
            let deck_id =
                campaign::normalized_deck_id(&f.db, &seat.main_deck, &seat.commander, &[]).unwrap();
            manifest.pod = std::array::from_fn(|_| deck_id.clone());
            rekey(&mut manifest);
            let output = f.root.join("complete-in-memory");
            let terminal = campaign::run_game(
                manifest,
                &output,
                f.db.clone(),
                state,
                header,
                &initial.events,
            )
            .unwrap();
            assert!(
                matches!(terminal["outcome"].as_str(), Some("won" | "draw")),
                "{terminal}"
            );
            let rows = records(&output);
            let mut observation = None;
            let mut action_count = 0;
            for row in &rows {
                match row["type"].as_str() {
                    Some("initial") => {
                        observation = Some(
                            serde_json::from_value::<Observation>(
                                row["payload"]["checkpoint"].clone(),
                            )
                            .unwrap(),
                        )
                    }
                    Some("action") => {
                        let spectator = &row["payload"]["spectator"];
                        if spectator.get("checkpoint").is_some() {
                            observation = Some(
                                serde_json::from_value(spectator["checkpoint"].clone()).unwrap(),
                            );
                        } else {
                            let delta: ObservationDelta =
                                serde_json::from_value(spectator["delta"].clone()).unwrap();
                            delta.apply(observation.as_mut().unwrap());
                        }
                        assert_eq!(
                            campaign::canonical_hash(
                                &serde_json::to_value(observation.as_ref().unwrap()).unwrap()
                            )
                            .unwrap(),
                            row["payload"]["observation_hash"]
                        );
                        if !row["payload"]["recorded_action"].is_null() {
                            action_count += 1;
                        }
                    }
                    _ => {}
                }
            }
            assert_eq!(terminal["actions"], action_count);
            assert!(terminal["per_card"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["draws"].as_u64().unwrap() > 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn evidence_limit_keeps_the_complete_prefix_and_allows_a_truthful_terminal() {
    let f = Fixture::new();
    let output = f.root.join("bounded");
    let mut writer = CampaignJournal::create(&f.manifest, &output).unwrap();
    writer
        .record("initial", json!({"checkpoint":{}}), true)
        .unwrap();
    let error = writer
        .record(
            "action",
            json!({"oversized":"x".repeat(campaign::OBSERVATION_LIMIT)}),
            false,
        )
        .unwrap_err();
    assert!(error.starts_with("evidence_limit/"));
    assert_eq!(
        writer
            .record(
                "terminal",
                json!({"outcome":"infrastructure_error","code":"evidence_limit","incomplete":true}),
                false
            )
            .unwrap(),
        1
    );
    let rows = records(&output);
    assert_eq!(rows.len(), 3); // unsequenced header, initial, terminal
    assert_eq!(rows[2]["seq"], 1);
}

#[test]
fn failure_restore_uses_engine_persistence_not_the_observation_dto() {
    let f = Fixture::new();
    let state = f.state();
    let output = f.root.join("restore-attempt");
    let header = engine::types::replay::ReplayHeader {
        format_config: engine::types::format::FormatConfig::commander(),
        match_config: Default::default(),
        player_count: 4,
        first_player: None,
        seed: 42,
        deck_data: None,
    };
    // Exercise the receipt encoder independently; this does not add a forced
    // outcome option to either production CLI or the engine driver.
    let recorder = campaign::CampaignRecorder::create(
        f.manifest.clone(),
        &output,
        &state,
        f.db.clone(),
        &header,
        &[],
    )
    .unwrap();
    recorder
        .finish(&state, "stalled", json!({"code":"test_receipt_encoding"}))
        .unwrap();
    let restored = phase_ai::saved_state::load_saved_game_state(
        &std::fs::read_to_string(output.join("restore.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(restored.rng_seed, state.rng_seed);
    assert_eq!(restored.rng.get_word_pos(), state.rng.get_word_pos());
    assert!(
        state.rng.get_word_pos() > 0,
        "fixture must have consumed entropy"
    );
    assert_eq!(restored.rng_word_pos, state.rng.get_word_pos());
    let mut expected_rng = state.rng.clone();
    let mut restored_rng = restored.rng.clone();
    for _ in 0..8 {
        assert_eq!(
            restored_rng
                .draw(engine::types::game_state::RandomDraw::Outcome)
                .random::<u64>(),
            expected_rng
                .draw(engine::types::game_state::RandomDraw::Outcome)
                .random::<u64>(),
            "restored stream must continue with the same future draws",
        );
    }
    assert_eq!(restored.players[0].hand, state.players[0].hand);
    let projected = serde_json::to_value(
        engine::game::derived_views::ClientGameStateRef::wrap_filtered(&state, &state, None),
    )
    .unwrap();
    let untrusted = json!({"gameState":projected["state"]});
    assert!(phase_ai::saved_state::load_saved_game_state(&untrusted.to_string()).is_err());
}

#[test]
fn fair_campaign_uses_materialized_choice_and_real_apply_with_exact_timing() {
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            let f = Fixture::new();
            let mut manifest = f.manifest.clone();
            manifest.information_model = campaign::InformationModel::OpenDecklistFairV1;
            manifest.fair_contract_version = 1;
            manifest.sampling_seed = Some("77".into());
            rekey(&mut manifest);
            let input = f.root.join("fair.json");
            std::fs::write(&input, serde_json::to_vec(&manifest).unwrap()).unwrap();
            let output = f.root.join("fair-attempt");
            let terminal = campaign::run_manifest(&input, &output).unwrap();
            assert_eq!(terminal["outcome"], "capped", "{terminal}");
            let rows = records(&output);
            let decision = &rows.iter().find(|r| r["type"] == "decision").unwrap()["payload"];
            assert_eq!(decision["information_model"], "open_decklist_fair_v1");
            assert_eq!(
                decision["latency_ns"].as_u64().unwrap(),
                decision["materialize_ns"].as_u64().unwrap()
                    + decision["chooser_ns"].as_u64().unwrap()
            );
            assert_eq!(rows.iter().filter(|r| r["type"] == "action").count(), 1);
        })
        .unwrap()
        .join()
        .unwrap();
}
