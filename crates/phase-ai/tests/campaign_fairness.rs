//! Native campaign fairness gates use deterministic in-memory rules fixtures.
//! These compare actions and synthetic state, never wall-clock durations.

use std::sync::Arc;

use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::deck_loading::DeckEntry;
use engine::game::effects::resolve_ability_chain;
use engine::game::printed_cards::apply_card_face_to_object;
use engine::game::visibility::known_object_ids_for_simulation;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DigSource, Effect, PtValue, QuantityExpr, ResolvedAbility,
    SearchSelectionConstraint, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, MulliganDecisionEntry, MulliganDecisionPhase, PlayerDeckPool,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::campaign_fair::{
    fair_decision_seed, materialize_fair_state, validate_sample_action, FairDecisionContext,
    FairDecisionError,
};
use phase_ai::config::{create_config, AiConfig, AiDifficulty, Platform};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn face(name: &str) -> CardFace {
    CardFace {
        name: name.to_owned(),
        mana_cost: ManaCost::zero(),
        ..Default::default()
    }
}

fn add(state: &mut GameState, player: PlayerId, card: &CardFace, zone: Zone) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        card.name.clone(),
        zone,
    );
    apply_card_face_to_object(state.objects.get_mut(&id).unwrap(), card);
    id
}

fn pool(state: &mut GameState, player: PlayerId, cards: &[CardFace]) {
    state.deck_pools.push(PlayerDeckPool {
        player,
        current_main: Arc::new(
            cards
                .iter()
                .cloned()
                .map(|card| DeckEntry { card, count: 1 })
                .collect(),
        ),
        ..Default::default()
    });
}

fn fixture() -> GameState {
    let mut state = GameState::new(engine::types::format::FormatConfig::commander(), 4, 987);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    for player in [P0, P1, PlayerId(2), PlayerId(3)] {
        let mut land = face(&format!("Fixture land {}", player.0));
        land.card_type = CardType {
            core_types: vec![CoreType::Land],
            ..Default::default()
        };
        let mut creature = face(&format!("Fixture creature {}", player.0));
        creature.card_type = CardType {
            core_types: vec![CoreType::Creature],
            ..Default::default()
        };
        creature.power = Some(PtValue::Fixed(2));
        creature.toughness = Some(PtValue::Fixed(2));
        let cards = [land, creature, face(&format!("Fixture other {}", player.0))];
        pool(&mut state, player, &cards);
        add(&mut state, player, &cards[0], Zone::Hand);
        add(&mut state, player, &cards[1], Zone::Library);
        add(&mut state, player, &cards[2], Zone::Library);
    }
    state
}

fn config() -> AiConfig {
    let mut config = create_config(AiDifficulty::Medium, Platform::Native).into_measurement(321);
    config.search.max_nodes = 8;
    config
}

fn choose(state: &GameState) -> Option<GameAction> {
    FairDecisionContext::new(123)
        .choose(state, &config(), P0, P0)
        .expect("fair decision")
        .action
}

fn substitute_hidden(state: &GameState) -> GameState {
    let mut changed = state.clone();
    let known = known_object_ids_for_simulation(state, P0);
    for (id, object) in changed.objects.iter_mut() {
        if !known.contains(id) {
            object.name = format!("Private replacement {}", id.0);
            object.base_name = object.name.clone();
            object.card_id = CardId(700_000 + id.0);
            object.intensity = 87;
            // Counters on face-down public objects remain observable knowledge;
            // only hidden hand/library metadata may change in this substitution.
            if matches!(object.zone, Zone::Hand | Zone::Library) {
                object.counters.insert(CounterType::Plus1Plus1, 9);
            }
            object.back_face = Some(engine::game::printed_cards::snapshot_object_face(
                &state.objects[id],
            ));
        }
    }
    changed.rng_seed = 999_999;
    changed.rng_word_pos = 12345;
    changed.rehydrate_rng();
    changed
}

#[test]
fn seed_is_fixed_width_domain_separated_sha256() {
    assert_eq!(
        fair_decision_seed(123, PlayerId(2), 7, 0),
        13_357_980_022_641_599_833
    );
    assert_ne!(
        fair_decision_seed(123, P0, 0, 0),
        fair_decision_seed(123, P0, 0, 1)
    );
    assert_ne!(
        fair_decision_seed(123, P0, 0, 0),
        fair_decision_seed(123, P1, 0, 0)
    );
}

#[test]
fn hidden_identity_rng_and_residual_metadata_do_not_change_state_or_action() {
    let state = fixture();
    let changed = substitute_hidden(&state);
    let left = materialize_fair_state(&state, P0, 77, None).unwrap();
    let right = materialize_fair_state(&changed, P0, 77, None).unwrap();
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
    assert_eq!(choose(&state), choose(&changed));
    assert!(choose(&state).is_some(), "exercise an actual choice");
}

#[test]
fn unknown_own_library_order_is_not_a_chooser_input() {
    let state = fixture();
    let mut changed = state.clone();
    changed.players[0].library = state.players[0].library.iter().copied().rev().collect();
    let left = materialize_fair_state(&state, P0, 77, None).unwrap();
    let right = materialize_fair_state(&changed, P0, 77, None).unwrap();
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
    assert_eq!(choose(&state), choose(&changed));
}

#[test]
fn synthetic_choice_does_not_mutate_real_state() {
    let state = fixture();
    let before = serde_json::to_value(&state).unwrap();
    choose(&state).expect("decision exists");
    assert_eq!(before, serde_json::to_value(&state).unwrap());
}

#[test]
fn every_unknown_object_is_reconstructed_not_patched() {
    let state = substitute_hidden(&fixture());
    let known = known_object_ids_for_simulation(&state, P0);
    let sample = materialize_fair_state(&state, P0, 77, None).unwrap();
    for (id, object) in &sample.objects {
        if known.contains(id) {
            continue;
        }
        assert_eq!(object.card_id, CardId((1u64 << 63) | id.0));
        assert!(object.counters.is_empty());
        assert!(object.perpetual_mods.is_empty());
        assert!(object.casting_permissions.is_empty());
        assert_eq!(object.intensity, 0);
        assert!(object.back_face.is_none());
        assert!(!object.name.starts_with("Private replacement"));
    }
    assert_eq!(sample.rng_seed, 77);
    assert_eq!(sample.rng_word_pos, 0);
}

#[test]
fn empty_or_short_pool_never_leaves_authoritative_cards() {
    let mut state = fixture();
    state.deck_pools[1].current_main = Arc::new(Vec::new());
    assert!(matches!(
        materialize_fair_state(&state, P0, 77, None),
        Err(FairDecisionError::HiddenPoolMismatch { .. })
    ));
    state.deck_pools.clear();
    assert!(matches!(
        materialize_fair_state(&state, P0, 77, None),
        Err(FairDecisionError::MissingDeckPool { .. })
    ));
}

#[test]
fn revealed_knowledge_is_retained_and_changes_materialization() {
    let state = fixture();
    let id = state.players[1].hand[0];
    let mut revealed = state.clone();
    revealed.revealed_cards.insert(id);
    let sample = materialize_fair_state(&revealed, P0, 77, None).unwrap();
    assert_eq!(sample.objects[&id].card_id, state.objects[&id].card_id);
    assert_eq!(sample.objects[&id].name, state.objects[&id].name);
    let unknown = materialize_fair_state(&state, P0, 77, None).unwrap();
    assert_ne!(unknown.objects[&id].card_id, sample.objects[&id].card_id);
}

#[test]
fn controlled_turn_uses_actor_private_visibility() {
    let mut state = fixture();
    state.active_player = P1;
    state.turn_decision_controller = Some(P0);
    state.waiting_for = WaitingFor::Priority { player: P1 };
    let hand = state.players[1].hand[0];
    let sample = materialize_fair_state(&state, P0, 77, None).unwrap();
    assert_eq!(sample.objects[&hand].card_id, state.objects[&hand].card_id);
    let decision = FairDecisionContext::new(123)
        .choose(&state, &config(), P1, P0)
        .unwrap();
    assert!(decision.action.is_some());
    assert!(matches!(
        FairDecisionContext::new(123).choose(&state, &config(), P1, P1),
        Err(FairDecisionError::InvalidActor)
    ));
}

#[test]
fn attacks_and_mulligans_are_sampled_before_specialists() {
    let mut state = fixture();
    state.turn_number = 2;
    state.phase = Phase::DeclareAttackers;
    let mut attacker = face("Fixture attacker");
    attacker.card_type = CardType {
        core_types: vec![CoreType::Creature],
        ..Default::default()
    };
    attacker.power = Some(PtValue::Fixed(4));
    attacker.toughness = Some(PtValue::Fixed(4));
    let id = add(&mut state, P0, &attacker, Zone::Battlefield);
    state.objects.get_mut(&id).unwrap().summoning_sick = false;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![id],
        valid_attack_targets: vec![AttackTarget::Player(P1)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    let action = choose(&state);
    assert!(matches!(action, Some(GameAction::DeclareAttackers { .. })));
    assert_eq!(action, choose(&substitute_hidden(&state)));
    state.waiting_for = WaitingFor::MulliganDecision {
        pending: vec![MulliganDecisionEntry {
            player: P0,
            mulligan_count: 0,
            free_reveals_taken: 0,
            phase: MulliganDecisionPhase::Declare,
        }],
        free_first_mulligan: true,
        declared: Vec::new(),
    };
    let action = choose(&state);
    assert!(matches!(action, Some(GameAction::MulliganDecision { .. })));
    assert_eq!(action, choose(&substitute_hidden(&state)));
}

#[test]
fn search_prompt_pins_looked_at_cards_and_uses_real_handles() {
    let mut state = fixture();
    let cards: Vec<_> = state.players[0].library.iter().copied().collect();
    state.waiting_for = WaitingFor::SearchChoice {
        player: P0,
        library_owner: Some(P0),
        cards: cards.clone(),
        count: 1,
        reveal: false,
        up_to: false,
        allows_partial_find: false,
        constraint: SearchSelectionConstraint::None,
        ordering_hint: Default::default(),
        split: None,
    };
    let sample = materialize_fair_state(&state, P0, 77, None).unwrap();
    for id in &cards {
        assert_eq!(sample.objects[id].card_id, state.objects[id].card_id);
    }
    let action = choose(&state);
    assert!(
        matches!(&action, Some(GameAction::SelectCards { cards: selected }) if selected.len() == 1 && cards.contains(&selected[0]))
    );
    assert_eq!(action, choose(&substitute_hidden(&state)));
}

#[test]
fn target_specialist_is_invariant_to_unknown_cards() {
    let mut state = fixture();
    let mut spell = face("Fixture targeting spell");
    spell.card_type = CardType {
        core_types: vec![CoreType::Sorcery],
        ..Default::default()
    };
    spell.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    )];
    Arc::make_mut(&mut state.deck_pools[0].current_main).push(DeckEntry {
        card: spell.clone(),
        count: 1,
    });
    let id = add(&mut state, P0, &spell, Zone::Hand);
    let cast = GameAction::CastSpell {
        object_id: id,
        card_id: state.objects[&id].card_id,
        targets: Vec::new(),
        payment_mode: CastPaymentMode::Auto,
    };
    engine::game::engine::apply_interaction(&mut state, P0, P0, cast)
        .expect("the real cast must create its stack entry and target continuation");
    assert!(
        matches!(state.waiting_for, WaitingFor::TargetSelection { .. }),
        "fixture must enter the real target-selection specialist: {:?}",
        state.waiting_for,
    );
    let action = choose(&state);
    assert!(
        matches!(action, Some(GameAction::ChooseTarget { .. })),
        "target specialist must select a real target, got {action:?}",
    );
    assert_eq!(action, choose(&substitute_hidden(&state)));
}

#[test]
fn durable_library_knowledge_expires_on_shuffle() {
    let mut state = fixture();
    let source = add(&mut state, P0, &face("Fixture observer"), Zone::Battlefield);
    let look = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Player,
            count: QuantityExpr::Fixed { value: 1 },
            destination: None,
            keep_count: Some(0),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            rest_split_top_count: None,
            rest_order: engine::types::ability::DigRestOrder::Preserve,
            reveal: false,
            enter_tapped: false,
            enters_attacking: false,
            source: DigSource::Library,
        },
        vec![TargetRef::Player(P1)],
        source,
        P0,
    );
    resolve_ability_chain(&mut state, &look, &mut Vec::new(), 0).unwrap();
    state.private_look_ids.clear();
    state.private_look_player = None;
    let known = known_object_ids_for_simulation(&state, P0);
    let learned: Vec<_> = state.players[1]
        .library
        .iter()
        .copied()
        .filter(|id| known.contains(id))
        .collect();
    assert!(!learned.is_empty());
    let sample = materialize_fair_state(&state, P0, 77, None).unwrap();
    for id in &learned {
        assert_eq!(sample.objects[id].card_id, state.objects[id].card_id);
    }
    let shuffle = ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::Player,
        },
        vec![TargetRef::Player(P1)],
        source,
        P0,
    );
    resolve_ability_chain(&mut state, &shuffle, &mut Vec::new(), 0).unwrap();
    let known = known_object_ids_for_simulation(&state, P0);
    assert!(learned.iter().all(|id| !known.contains(id)));
    assert_eq!(choose(&state), choose(&substitute_hidden(&state)));
}

#[test]
fn alternate_faces_come_from_pinned_database_not_real_object() {
    let mut front = face("Fixture front");
    front.scryfall_oracle_id = Some("fixture-modal".into());
    let mut back = face("Fixture back");
    back.scryfall_oracle_id = front.scryfall_oracle_id.clone();
    let mut front_json = serde_json::to_value(&front).unwrap();
    front_json["layout"] = "modal_dfc".into();
    front_json["face_index"] = 0.into();
    let mut back_json = serde_json::to_value(&back).unwrap();
    back_json["layout"] = "modal_dfc".into();
    back_json["face_index"] = 1.into();
    let database = CardDatabase::from_json_str(
        &serde_json::json!({"fixture front": front_json, "fixture back": back_json}).to_string(),
    )
    .unwrap();
    let mut state = GameState::new_two_player(42);
    pool(&mut state, P1, &[front.clone()]);
    let id = add(&mut state, P1, &front, Zone::Library);
    assert!(matches!(
        materialize_fair_state(&state, P0, 77, None),
        Err(FairDecisionError::MissingCardDatabase { .. })
    ));
    let sample = materialize_fair_state(&state, P0, 77, Some(&database)).unwrap();
    assert_eq!(
        sample.objects[&id].back_face.as_ref().unwrap().name,
        back.name
    );
    let changed = substitute_hidden(&state);
    let changed_sample = materialize_fair_state(&changed, P0, 77, Some(&database)).unwrap();
    assert_eq!(
        serde_json::to_value(&sample).unwrap(),
        serde_json::to_value(&changed_sample).unwrap()
    );
}

#[test]
fn real_contract_rejects_sampled_private_identity_without_fallback() {
    let state = fixture();
    let id = state.players[1].hand[0];
    let action = GameAction::PlayLand {
        object_id: id,
        card_id: CardId((1u64 << 63) | id.0),
    };
    let error = validate_sample_action(&state, P0, P0, &action).unwrap_err();
    assert_eq!(error, FairDecisionError::RejectedSampleAction);
    assert_eq!(error.outcome(), "fair_protocol");
    assert_eq!(error.code(), "rejected_sample_action");
}

#[test]
fn inner_ensemble_config_cannot_reintroduce_real_rng_or_shared_sessions() {
    let state = fixture();
    let mut outer = config();
    outer.search.determinization_samples = 4;
    let mut context = FairDecisionContext::new(123);
    let first = context.choose(&state, &outer, P0, P0).unwrap().action;
    assert_eq!(first, choose(&state));
    let mut fresh = FairDecisionContext::new(123);
    assert_eq!(
        first,
        fresh
            .choose(&substitute_hidden(&state), &outer, P0, P0)
            .unwrap()
            .action
    );
}

#[test]
fn spell_selection_is_invariant_to_hidden_world() {
    let mut state = fixture();
    let land = state.players[0].hand[0];
    let play_land = GameAction::PlayLand {
        object_id: land,
        card_id: state.objects[&land].card_id,
    };
    engine::game::engine::apply_interaction(&mut state, P0, P0, play_land)
        .expect("consume the fixture's land play through the real engine");
    assert_eq!(state.objects[&land].zone, Zone::Battlefield);
    assert!(!state.players[0].hand.contains(&land));
    let mut spell = face("Fixture terminal spell");
    spell.card_type = CardType {
        core_types: vec![CoreType::Sorcery],
        ..Default::default()
    };
    // A real terminal effect forces the spell branch independently of whether
    // a particular heuristic prefers developing a vanilla creature or passing.
    spell.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::WinTheGame { target: None },
    )];
    Arc::make_mut(&mut state.deck_pools[0].current_main).push(DeckEntry {
        card: spell.clone(),
        count: 1,
    });
    let id = add(&mut state, P0, &spell, Zone::Hand);
    let expected = GameAction::CastSpell {
        object_id: id,
        card_id: state.objects[&id].card_id,
        targets: Vec::new(),
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(
        engine::ai_support::AiDecisionContract::issue(&state, P0).permits(&state, P0, &expected),
        "terminal spell must be in the actual engine contract",
    );
    let action = choose(&state);
    assert!(
        matches!(&action, Some(GameAction::CastSpell { object_id, .. }) if *object_id == id),
        "terminal spell must be cast, got {action:?}",
    );
    assert_eq!(action, choose(&substitute_hidden(&state)));
}

#[test]
fn hidden_lki_side_tables_are_not_carried_into_sample() {
    let mut state = fixture();
    let id = state.players[1].hand[0];
    state
        .lki_cache
        .insert(id, state.objects[&id].snapshot_public_characteristics());
    let mut changed = substitute_hidden(&state);
    changed
        .lki_cache
        .insert(id, changed.objects[&id].snapshot_public_characteristics());
    let left = materialize_fair_state(&state, P0, 77, None).unwrap();
    let right = materialize_fair_state(&changed, P0, 77, None).unwrap();
    assert!(!left.lki_cache.contains_key(&id));
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
}

#[test]
fn unknown_non_deck_card_is_explicit_harness_evidence() {
    let mut state = fixture();
    add(
        &mut state,
        P1,
        &face("Unregistered generated card"),
        Zone::Hand,
    );
    let error = materialize_fair_state(&state, P0, 77, None).unwrap_err();
    assert!(matches!(
        error,
        FairDecisionError::HiddenPoolMismatch { .. }
    ));
    assert_eq!(error.outcome(), "fair_protocol");
}

#[test]
fn companion_enters_hidden_pool_only_after_moving_into_game() {
    let mut state = fixture();
    let companion = face("Fixture companion");
    state.players[1].companion = Some(engine::types::player::CompanionInfo {
        card: DeckEntry {
            card: companion.clone(),
            count: 1,
        },
        used: false,
        printing: Default::default(),
    });
    materialize_fair_state(&state, P0, 77, None)
        .expect("declared companion is still outside the game");
    let mut without_knowledge = state.clone();
    without_knowledge.players[1]
        .companion
        .as_mut()
        .unwrap()
        .used = true;
    add(&mut without_knowledge, P1, &companion, Zone::Hand);
    materialize_fair_state(&without_knowledge, P0, 77, None)
        .expect("the used companion also balances a snapshot without prior knowledge");

    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    let source = add(
        &mut state,
        P1,
        &face("Fixture mana source"),
        Zone::Battlefield,
    );
    for _ in 0..3 {
        state
            .add_mana_to_pool(
                P1,
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    source,
                    false,
                    Vec::new(),
                ),
            )
            .expect("fixture player accepts a mana unit");
    }
    let result =
        engine::game::engine::apply_interaction(&mut state, P1, P1, GameAction::CompanionToHand)
            .expect("the real companion action pays and creates the hand occurrence");
    assert!(result.events.iter().any(|event| matches!(
        event,
        engine::types::events::GameEvent::CompanionMovedToHand { player, card_name }
            if *player == P1 && card_name == &companion.name
    )));
    assert!(state.players[1].companion.as_ref().unwrap().used);
    let id = *state.players[1]
        .hand
        .iter()
        .find(|id| state.objects[*id].name == companion.name)
        .expect("the companion action created its card");
    for viewer in [P0, P1, PlayerId(2), PlayerId(3)] {
        assert!(
            known_object_ids_for_simulation(&state, viewer).contains(&id),
            "the public move must record knowledge for viewer {viewer:?}",
        );
    }
    let sample = materialize_fair_state(&state, P0, 77, None)
        .expect("known companion cancels its added pool count instead of adding a hidden slot");
    assert_eq!(sample.objects[&id].card_id, state.objects[&id].card_id);
    assert_eq!(sample.objects[&id].name, companion.name);
}

fn assert_face_down_sampling_preserves_public_fields(zone: Zone) {
    let mut state = fixture();
    let mut private_face = face("Fixture private face");
    private_face.card_type = CardType {
        core_types: vec![CoreType::Creature],
        ..Default::default()
    };
    private_face.power = Some(PtValue::Fixed(7));
    private_face.toughness = Some(PtValue::Fixed(7));
    Arc::make_mut(&mut state.deck_pools[1].current_main).push(DeckEntry {
        card: private_face.clone(),
        count: 1,
    });
    let id = add(&mut state, P1, &private_face, zone);
    if zone == Zone::Battlefield {
        let object = state.objects.get_mut(&id).unwrap();
        let reverse = engine::game::printed_cards::snapshot_object_face(object);
        engine::game::morph::apply_face_down_creature_characteristics(
            object,
            &engine::types::ability::FaceDownProfile::vanilla_2_2(),
        );
        object.back_face = Some(reverse);
        object.tapped = true;
        object.damage_marked = 1;
        object.counters.insert(CounterType::Plus1Plus1, 1);

        let mut equipment = face("Fixture public equipment");
        equipment.card_type = CardType {
            core_types: vec![CoreType::Artifact],
            subtypes: vec!["Equipment".into()],
            ..Default::default()
        };
        let attachment = add(&mut state, P1, &equipment, Zone::Battlefield);
        state.objects.get_mut(&attachment).unwrap().attached_to =
            Some(engine::game::game_object::AttachTarget::Object(id));
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .attachments
            .push(attachment);
    } else {
        assert_eq!(zone, Zone::Exile);
        state.objects.get_mut(&id).unwrap().face_down = true;
    }
    assert!(!known_object_ids_for_simulation(&state, P0).contains(&id));
    let public = engine::game::visibility::filter_state_for_viewer(&state, P0);
    let changed = substitute_hidden(&state);
    assert_ne!(changed.objects[&id].card_id, state.objects[&id].card_id);
    let sample = materialize_fair_state(&state, P0, 77, None).unwrap();
    let changed_sample = materialize_fair_state(&changed, P0, 77, None).unwrap();
    assert_eq!(
        serde_json::to_value(&sample).unwrap(),
        serde_json::to_value(&changed_sample).unwrap()
    );
    let sampled = &sample.objects[&id];
    let projected = &public.objects[&id];
    assert_eq!(sampled.card_id, CardId((1u64 << 63) | id.0));
    assert_eq!(sampled.face_down, projected.face_down);
    assert_eq!(sampled.counters, projected.counters);
    assert_eq!(sampled.tapped, projected.tapped);
    assert_eq!(sampled.damage_marked, projected.damage_marked);
    assert_eq!(sampled.attached_to, projected.attached_to);
    assert_eq!(sampled.attachments, projected.attachments);
    let action = choose(&state);
    assert!(
        action.is_some(),
        "the face-down fixture must exercise a real decision"
    );
    assert_eq!(action, choose(&changed));
}

#[test]
fn face_down_battlefield_sampling_preserves_public_status_and_attachments() {
    assert_face_down_sampling_preserves_public_fields(Zone::Battlefield);
}

#[test]
fn face_down_exile_sampling_is_independent_of_private_identity() {
    assert_face_down_sampling_preserves_public_fields(Zone::Exile);
}

#[test]
fn materialization_invariant_errors_remain_fair_protocol() {
    let missing = FairDecisionError::MissingPublicProjection {
        object: ObjectId(1),
    };
    assert_eq!(missing.outcome(), "fair_protocol");
    assert_eq!(missing.code(), "missing_public_projection");
    let mismatch = FairDecisionError::HiddenSlotCardinalityMismatch {
        player: P0,
        zone: Zone::Library,
    };
    assert_eq!(mismatch.outcome(), "fair_protocol");
    assert_eq!(mismatch.code(), "hidden_slot_cardinality");
}
