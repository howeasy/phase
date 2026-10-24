//! Rules identity of display-only data: the consumed trigger-event occurrence relation.
//!
//! CR 603.2c: an ability triggers once each time its trigger event occurs, and the
//! consumed-occurrence journal is how a second collector is kept from triggering it again
//! for an occurrence the first already collected. An occurrence is a pair (event, ordinal),
//! where the ordinal counts the earlier events of the whole action buffer that are typed
//! `==` to it (`triggers::trigger_event_occurrence`). Every consumer matches claims with
//! that relation, so every producer of claims must number with it. The stack batch paths
//! numbered by serialized string instead; a pair that is typed-equal but serializes apart
//! (a reported life total, a display-only printing; CR 203.1 + CR 206.1: a printing has no
//! effect on game play) was given ordinals the consumers read differently, leaving a
//! collected occurrence collectable.
//!
//! Where typed equality and serialized identity disagree. Typed-equal but serialized apart:
//! the always-equal `LifeTotalReading` in `LifeChanged`, and every `DisplayOnly` printing
//! carrier reachable from a `GameEvent` (the look-back snapshot in `ZoneChangeRecord` and
//! `AbilityActivated`, `LibrarySearchCardView`). Serialized-equal but typed apart:
//! `CounterType::Loyalty` against `CounterType::Generic("loyalty")` in `CounterAdded`.
//! `ResolvedAbility` equality compares the aligned target pins (it does not ignore
//! `target_pins`); its separate divergence is the plural `cost_paid_objects`, compared by
//! object-id sequence only. The producer takes its ordinals from the consumers' relation, so
//! all of these are consumer-consistent by construction.
//!
//! The numbering change is intentional: wherever the two relations disagree, the producer's
//! ordinals now are the consumers'. A non-empty prefix of the caller's buffer that is equal
//! to a claimed event also changes the ordinals a locally numbered producer would write, even
//! when serialization and typed equality agree; the claims are exact for the whole buffer.
//!
//! Persistence and replay. The ledger is serialized with the game state. Persisted
//! reconciliation visits it as a live event carrier but compares raw-JSON zone-change
//! fingerprints, which is a separate authority and not a second ordinal allocator. Action
//! replay clones checkpoints and calls the reducer, so it never renumbers a claim, and
//! applying a `ConsumeBeforePriority` collection extends the claims unchanged.
//!
//! Cost. On the measured native fixtures, equality numbering cost approximately 0.5% or less
//! of the measured resolver time. The measured single-token runs produced four events per
//! member. WASM timing and other admitted event-volume shapes were not measured.
//!
//! Casting reversal (CR 733.1 + CR 104.4b): the descriptor keeps a restoration list (the exact
//! inverse, printing included, the only list a cancel applies) and a comparison list (the
//! inverse of the rules projection), recorded together at every capture point; two states
//! whose reversals differ only in display-only data are the same state, a descriptor saved
//! before the comparison list existed compares by its restoration list and never equals a new
//! one.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::stack::resolve_next_with_limit;
use engine::game::triggers::ConsumedTriggerEventScope;
use engine::types::ability::{AbilityCost, QuantityExpr, StaticDefinition};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_printing::CardPrinting;
use engine::types::display_only::{rules_projection, DisplayOnly};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassRequest, CastPaymentMode, GameState, PersistedGameState,
    StackResolutionAutoPassOverlay, StackResolutionBudget, StackResolutionEntryFence,
    StackResolutionPolicy, StackResolutionSession, TrustedGameStateEnvelope, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{AdditionalCostTaxAction, StaticMode};
use engine::types::zones::Zone;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const BOGWATER_ORACLE: &str =
    "Whenever this creature or another creature you control enters, you gain 1 life.";

/// Synthetic spell: no printed card is claimed, it only produces the five token entries.
const TOKEN_FLOOD_ORACLE: &str = "Create five 1/1 green Insect creature tokens.";

const TOKEN_COUNT: usize = 5;

/// The test-local oracle for the consumers' relation: how many earlier events of the
/// buffer are typed `==` to each event.
fn equality_occurrences(events: &[GameEvent]) -> Vec<usize> {
    (0..events.len())
        .map(|index| {
            events[..index]
                .iter()
                .filter(|earlier| **earlier == events[index])
                .count()
        })
        .collect()
}

/// The committed session Resolve All authorizes, fenced over the whole stack.
fn arm_committed_session(state: &mut GameState) {
    state.stack_resolution_session = Some(StackResolutionSession {
        entries: state
            .stack
            .iter()
            .rev()
            .map(StackResolutionEntryFence::capture)
            .collect(),
        cursor: 0,
        representatives: BTreeSet::from([P0]),
        verified_pass_representatives: BTreeSet::new(),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::Committed,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: BTreeMap::new(),
        },
    });
}

/// CR 603.2c + CR 119.3: five tokens enter under Bogwater Lumaret, so five identical
/// "you gain 1 life" triggers resolve as one batch. Each gain is one occurrence of the same
/// typed event (the reported totals 21..25 are display-only), so the batch must claim them
/// as ordinals 0..4 and the caller's checkpoint must consume every one.
#[test]
fn bogwater_life_gain_batch_claims_every_gain_at_its_consumer_occurrence() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.add_creature_from_oracle(P0, "Bogwater Lumaret", 2, 2, BOGWATER_ORACLE);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Token Flood", true, TOKEN_FLOOD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast token flood");
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("the spell resolves through a committed stack session");
    assert_eq!(
        runner.state().stack.len(),
        TOKEN_COUNT,
        "reach: the spell left exactly its five life-gain triggers"
    );

    // Resolve the five triggers through the production resolver, with the session Resolve
    // All installs, and read the claims before the post-action pipeline takes them.
    let mut state = runner.state().clone();
    arm_committed_session(&mut state);
    let mut events = Vec::new();
    assert_eq!(
        resolve_next_with_limit(&mut state, &mut events, Some(u32::MAX)),
        TOKEN_COUNT as u32
    );

    let totals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged { new_total, .. } => Some(new_total.0),
            _ => None,
        })
        .collect();
    assert_eq!(totals, (21..=25).map(Some).collect::<Vec<_>>());
    assert_eq!(state.players[0].life, 25);

    let claims = &state.consumed_before_priority_trigger_events;
    assert_eq!(claims.len(), events.len(), "every batch event is claimed");
    assert!(
        claims
            .iter()
            .all(|claim| claim.scope == ConsumedTriggerEventScope::AllCollectors),
        "the batch claims for every collector"
    );
    assert_eq!(
        claims
            .iter()
            .map(|claim| claim.occurrence)
            .collect::<Vec<_>>(),
        equality_occurrences(&events),
        "each ordinal is the one the consumers number the buffer with"
    );
    for (claim, event) in claims.iter().zip(&events) {
        assert_eq!(claim.event, *event);
    }
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim.event, GameEvent::LifeChanged { .. }))
            .map(|claim| claim.occurrence)
            .collect::<Vec<_>>(),
        (0..TOKEN_COUNT).collect::<Vec<_>>(),
        "the five gains are five occurrences of one event"
    );
}

// ---------------------------------------------------------------------------
// Casting reversal: restoration list and comparison list
// ---------------------------------------------------------------------------

const BITTER_TRIUMPH_ORACLE: &str = "As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker.";

fn printing(set_code: &str) -> DisplayOnly<Option<CardPrinting>> {
    DisplayOnly::new(Some(CardPrinting {
        set_code: set_code.to_string(),
        collector_number: "1".to_string(),
    }))
}

fn stamp(state: &mut GameState, id: ObjectId, set_code: &str) {
    let object = state
        .objects
        .get_mut(&id)
        .expect("the stamped object exists");
    object.printing = printing(set_code);
    object.base_printing = printing(set_code);
}

fn set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

fn base_set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .base_printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

/// Number of nodes whose `set_code` is `code`.
fn sentinels(value: &Value, code: &str) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.get("set_code").and_then(Value::as_str) == Some(code))
                + map
                    .values()
                    .map(|child| sentinels(child, code))
                    .sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|child| sentinels(child, code)).sum(),
        _ => 0,
    }
}

/// The descriptor's JSON (the descriptor is crate-private, so tests read its wire form).
fn reversal(state: &GameState) -> Value {
    serde_json::to_value(state).expect("state serializes")["spell_cast_reversal"].clone()
}

/// The path of one serialized cast-undo edit (`{"Value": {"path": [..], ..}}`).
fn edit_path(edit: &Value) -> Vec<String> {
    edit.as_object()
        .and_then(|variants| variants.values().next())
        .and_then(|body| body["path"].as_array())
        .expect("an edit carries its path")
        .iter()
        .map(|part| part.as_str().expect("path parts are strings").to_string())
        .collect()
}

fn edits(list: &Value) -> &Vec<Value> {
    list.as_array().expect("an edit list is an array")
}

fn holds_path(list: &Value, path: &[&str]) -> bool {
    edits(list).iter().any(|edit| edit_path(edit) == path)
}

fn is_printing_edit_of(edit: &Value, id: ObjectId) -> bool {
    let path = edit_path(edit);
    let key = id.0.to_string();
    path.len() == 3
        && path[0] == "objects"
        && path[1] == key
        && (path[2] == "printing" || path[2] == "base_printing")
}

fn without_printing_edits_of(list: &Value, id: ObjectId) -> Vec<Value> {
    edits(list)
        .iter()
        .filter(|edit| !is_printing_edit_of(edit, id))
        .cloned()
        .collect()
}

fn fund(runner: &mut GameRunner, amount: usize) {
    for _ in 0..amount {
        runner
            .state_mut()
            .add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            )
            .expect("the player has a mana pool");
    }
}

fn tap(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine offers the land's mana ability while paying");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the land");
}

/// A persisted decode path: the state as written, read back.
type Reload = fn(&GameState) -> GameState;

fn reload_raw(state: &GameState) -> GameState {
    serde_json::from_str(&serde_json::to_string(state).expect("state serializes"))
        .expect("the raw decoder accepts a state it wrote")
}

fn reload_trusted(state: &GameState) -> GameState {
    let text = serde_json::to_string(&TrustedGameStateEnvelope::capture(state.clone()))
        .expect("envelope serializes");
    serde_json::from_str::<TrustedGameStateEnvelope>(&text)
        .expect("the trusted decoder accepts an envelope it wrote")
        .into_game_state()
}

/// The raw decode of `state`'s JSON after `edit` rewrote it.
fn raw_decode_after(state: &GameState, edit: impl FnOnce(&mut Value)) -> GameState {
    let mut json = serde_json::to_value(state).expect("state serializes");
    edit(&mut json);
    serde_json::from_value(json).expect("the edited state decodes")
}

fn without_reversal(state: &GameState) -> GameState {
    raw_decode_after(state, |json| {
        json.as_object_mut()
            .expect("a state is an object")
            .remove("spell_cast_reversal");
    })
}

/// A descriptor saved before the comparison list existed: the same state with that key gone.
fn legacy_of(state: &GameState) -> GameState {
    raw_decode_after(state, |json| {
        let descriptor = json["spell_cast_reversal"]
            .as_object_mut()
            .expect("reach: the state holds a descriptor");
        assert!(
            descriptor.remove("comparison_undo").is_some(),
            "reach: the descriptor held a comparison list"
        );
    })
}

/// A morph card in P0's hand and an opposing synthetic source of an unconditional "cast a
/// spell: pay 2 life" tax (no printed card is claimed), so one action both pays a cost and
/// conceals the printing. Returns the runner paused at mana payment after the face-down
/// choice, the morph card and P0's lands.
fn face_down_under_tax(players: u8, code: &str) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 9771)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let tax = scenario.add_creature(P1, "Tax Source", 1, 1).id();
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    stamp(runner.state_mut(), morph, code);
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&tax)
            .expect("the tax source exists");
        let definition = StaticDefinition::new(StaticMode::ImposeAdditionalCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            spell_filter: None,
            action: AdditionalCostTaxAction::Cast,
        });
        object.static_definitions.push(definition.clone());
        std::sync::Arc::make_mut(&mut object.base_static_definitions).push(definition);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    fund(&mut runner, 5);
    let card_id = runner.state().objects[&morph].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: morph,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce the morph card");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "reach: the face-down choice is pending, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choose the face-down cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].life,
        18,
        "reach: the tax was paid by the action that concealed the printing"
    );
    (runner, morph, lands)
}

/// CR 733.1: a cancelled cast returns the card to hand face up, with its real name and the
/// printing it held before the cast, inner values included.
fn assert_cancelled_with_printing(runner: &GameRunner, morph: ObjectId, code: &str) {
    let object = &runner.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand, "reach: back in hand");
    assert!(!object.face_down, "reach: face up again");
    assert_eq!(object.name, "Secret Beast", "reach: the real name is back");
    assert_eq!(set_code(runner.state(), morph).as_deref(), Some(code));
    assert_eq!(base_set_code(runner.state(), morph).as_deref(), Some(code));
    assert_eq!(
        runner.state().players[0].life,
        20,
        "reach: the tax is refunded"
    );
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 104.4b + CR 203.1 + CR 206.1: two boards whose casting reversals differ only in the
/// stamped printing are the same game state. The restoration list differs (it must return each
/// printing on cancel); the comparison list is the same and holds no printing.
#[test]
fn face_down_casts_whose_reversals_differ_only_in_a_printing_are_one_state() {
    let (a, morph, _) = face_down_under_tax(2, "SAAA");
    let (b, morph_b, _) = face_down_under_tax(2, "SBBB");
    assert_eq!(morph, morph_b, "reach: the boards are the same board");
    let (ra, rb) = (reversal(a.state()), reversal(b.state()));

    assert_eq!(
        ra["cost_mutations"], true,
        "reach: a cost mutation is recorded"
    );
    assert_ne!(
        ra["undo"], rb["undo"],
        "reach: the operands differ in the descriptor"
    );
    assert_eq!(sentinels(&ra["undo"], "SAAA"), 2, "{ra}");
    assert_eq!(sentinels(&rb["undo"], "SBBB"), 2, "{rb}");
    for field in ["printing", "base_printing"] {
        assert!(
            holds_path(&ra["undo"], &["objects", &morph.0.to_string(), field]),
            "reach: the restoration list holds the {field} edit: {ra}"
        );
    }

    let comparison = &ra["comparison_undo"];
    assert!(
        !edits(comparison).is_empty(),
        "reach: the comparison list records the paid cost: {ra}"
    );
    assert_eq!(comparison, &rb["comparison_undo"]);
    for code in ["SAAA", "SBBB"] {
        assert_eq!(
            sentinels(comparison, code),
            0,
            "the comparison list holds {code}"
        );
    }
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals are one state"
    );

    // A live twin with the same printing is equal too: the boards are deterministic, so the
    // descriptor was the only separating field.
    let (twin, _, _) = face_down_under_tax(2, "SAAA");
    assert!(a.state() == twin.state());
}

/// CR 104.4b: a difference in the rules identity still separates states. Both operands go
/// through the same decode, which resets runtime-only fields a live state does not share.
#[test]
fn a_rules_difference_in_the_casting_reversal_still_separates_states() {
    let (a, _, _) = face_down_under_tax(2, "SAAA");
    let plain = reload_raw(a.state());
    assert!(
        plain == reload_raw(a.state()),
        "reach: two decodes of one state are equal"
    );

    let edited = raw_decode_after(a.state(), |json| {
        let list = json["spell_cast_reversal"]["comparison_undo"]
            .as_array_mut()
            .expect("the comparison list is present");
        let edit = list
            .iter_mut()
            .find(|edit| edit_path(edit) == ["players", "0", "life"])
            .expect("reach: the paid life is a comparison edit");
        assert_eq!(edit["Value"]["before"], 20);
        edit["Value"]["before"] = Value::from(19);
    });
    assert!(
        edited != plain,
        "CR 104.4b: a rules difference in the comparison list separates states"
    );
    assert!(
        without_reversal(&edited) == without_reversal(&plain),
        "reach: the descriptor is the only difference"
    );
}

/// CR 733.1 + CR 104.4b: a second payment folds the prior inverse into both lists, so the
/// identity stays printing-free and a cancel still restores the printing exactly.
#[test]
fn a_later_payment_folds_both_lists_and_cancel_restores_the_printing() {
    let (mut a, morph, lands) = face_down_under_tax(2, "SAAA");
    let (mut b, _, _) = face_down_under_tax(2, "SBBB");
    let (undo_before, comparison_before) = {
        let r = reversal(a.state());
        (r["undo"].clone(), r["comparison_undo"].clone())
    };

    tap(&mut a, lands[0]);
    tap(&mut b, lands[0]);
    let (ra, rb) = (reversal(a.state()), reversal(b.state()));
    for runner in [&a, &b] {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "reach: still paying, got {:?}",
            runner.state().waiting_for
        );
    }
    assert_ne!(
        ra["undo"], undo_before,
        "reach: the fold rewrote the restoration list"
    );
    assert_ne!(
        ra["comparison_undo"], comparison_before,
        "reach: the fold rewrote the comparison list"
    );
    assert_eq!(
        sentinels(&ra["undo"], "SAAA"),
        2,
        "the printing is still restorable"
    );
    assert_eq!(sentinels(&ra["comparison_undo"], "SAAA"), 0);
    assert_eq!(ra["comparison_undo"], rb["comparison_undo"]);
    // The fold witness: the folded comparison list is the folded restoration list minus
    // exactly its two printing edits.
    assert_eq!(
        edits(&ra["undo"])
            .iter()
            .filter(|edit| is_printing_edit_of(edit, morph))
            .count(),
        2,
        "the face-down printing is the only one: {ra}"
    );
    assert_eq!(
        edits(&ra["comparison_undo"]),
        &without_printing_edits_of(&ra["undo"], morph),
        "the comparison list was folded with the restoration list"
    );
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals stay one state after a fold"
    );

    a.act(GameAction::CancelCast).expect("cancel A");
    assert_cancelled_with_printing(&a, morph, "SAAA");
    b.act(GameAction::CancelCast).expect("cancel B");
    assert_cancelled_with_printing(&b, morph, "SBBB");
}

/// CR 733.1 + CR 800.4a: a third player's concession is rebased into both lists, and cancelling
/// the spell neither restores that player nor loses the printing.
#[test]
fn a_concession_rebases_both_lists_and_cancel_keeps_it() {
    let p2 = PlayerId(2);
    let (mut a, morph, _) = face_down_under_tax(3, "SKAA");
    let (mut b, _, _) = face_down_under_tax(3, "SKBB");
    let before = reversal(a.state());

    for runner in [&mut a, &mut b] {
        runner
            .act(GameAction::Concede { player_id: p2 })
            .expect("the third player concedes independently");
        assert!(runner.state().players[2].is_eliminated, "reach: P2 is out");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
    }
    let after = reversal(a.state());
    assert_ne!(
        after["undo"], before["undo"],
        "reach: the rebase rewrote the restoration list"
    );
    assert_eq!(sentinels(&after["undo"], "SKAA"), 2);
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals stay one state after a rebase"
    );

    // The rebase witness: the comparison list is the rebased restoration list minus exactly
    // its two printing edits, and it moved with the rebase.
    let printing_edits = edits(&after["undo"])
        .iter()
        .filter(|edit| is_printing_edit_of(edit, morph))
        .count();
    assert_eq!(
        printing_edits, 2,
        "the face-down printing is the only one: {after}"
    );
    assert_eq!(
        edits(&after["comparison_undo"]),
        &without_printing_edits_of(&after["undo"], morph),
        "the comparison list was rebased with the restoration list"
    );
    assert_ne!(
        after["comparison_undo"], before["comparison_undo"],
        "the pre-concession comparison list is stale after the concession"
    );

    a.act(GameAction::CancelCast).expect("cancel the spell");
    assert!(
        a.state().players[2].is_eliminated,
        "CR 800.4a: the concession is not a casting payment"
    );
    assert_cancelled_with_printing(&a, morph, "SKAA");
}

fn discard_cast(code: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Target", 3, 3);
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    let spell = scenario
        .add_spell_to_hand(P0, "Bitter Triumph", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .from_oracle_text(BITTER_TRIUMPH_ORACLE)
        .id();
    let mut runner = scenario.build();
    stamp(runner.state_mut(), fodder, code);
    for _ in 0..2 {
        runner
            .state_mut()
            .add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            )
            .expect("the player has a mana pool");
    }
    (runner, spell, fodder)
}

fn pay_by_discarding(runner: &mut GameRunner, spell: ObjectId, fodder: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Bitter Triumph");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "reach: the additional cost is offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("choose to discard");
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("discard the fodder");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 733.1: a cost that moves a printed card (an ordinary move, not a concealment) is
/// restored exactly: the card returns to hand with its printing, its zone-change record is
/// gone, and the hand is as it was.
#[test]
fn cancelling_a_discard_paid_cast_returns_the_printed_card_exactly() {
    let (mut runner, spell, fodder) = discard_cast("STAA");
    let hand_before = runner.state().players[0].hand.clone();
    let records_before =
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize");
    assert_eq!(
        edits(&records_before).len(),
        0,
        "reach: no record before the cast"
    );

    pay_by_discarding(&mut runner, spell, fodder);
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach: discarded"
    );
    let descriptor = reversal(runner.state());
    assert_eq!(
        descriptor["cost_mutations"], true,
        "reach: the discard is a cost mutation"
    );
    assert_eq!(
        sentinels(&descriptor["undo"], "STAA"),
        0,
        "an ordinary move puts no printing into the restoration list"
    );
    let records =
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize");
    assert_eq!(
        edits(&records).len(),
        1,
        "reach: one zone-change record: {records}"
    );
    assert_eq!(
        sentinels(&records, "STAA"),
        1,
        "reach: its look-back snapshot carries the printing"
    );

    // Twins that differ only in the discarded card's printing are one state.
    let (mut twin, twin_spell, twin_fodder) = discard_cast("STBB");
    pay_by_discarding(&mut twin, twin_spell, twin_fodder);
    assert!(
        runner.state() == twin.state(),
        "CR 104.4b: printing-only discard casts are one state"
    );

    runner.act(GameAction::CancelCast).expect("cancel the cast");
    let object = &runner.state().objects[&fodder];
    assert_eq!(
        object.zone,
        Zone::Hand,
        "CR 733.1: the discarded card is back"
    );
    assert_eq!(set_code(runner.state(), fodder).as_deref(), Some("STAA"));
    assert_eq!(
        base_set_code(runner.state(), fodder).as_deref(),
        Some("STAA")
    );
    assert_eq!(
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize"),
        records_before,
        "the cancelled cast leaves no zone-change record"
    );
    assert!(runner.state().players[0].hand == hand_before);
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 733.1 + CR 104.4b: a persisted state keeps both lists on the raw and the trusted decode
/// paths, still compares by the identity, and a reloaded cancel restores the printing.
#[test]
fn a_reloaded_reversal_keeps_both_lists_and_compares_the_same() {
    let (a, morph, _) = face_down_under_tax(2, "SAAA");
    let (b, _, _) = face_down_under_tax(2, "SBBB");
    let original = reversal(a.state());
    assert!(
        !edits(&original["comparison_undo"]).is_empty(),
        "reach: a comparison list is saved"
    );
    assert_eq!(sentinels(&original["undo"], "SAAA"), 2);

    let paths: [(&str, Reload); 2] = [("raw", reload_raw), ("trusted", reload_trusted)];
    for (label, reload) in paths {
        let reloaded_a = reload(a.state());
        let reloaded_b = reload(b.state());
        assert_eq!(
            reversal(&reloaded_a),
            original,
            "{label}: the descriptor survives the reload byte for byte"
        );
        assert!(
            reload(a.state()) == reloaded_a,
            "{label}: two decodes of one state are equal"
        );
        assert!(
            without_reversal(&reloaded_a) == without_reversal(&reloaded_b),
            "{label}: reach: the descriptor is the only difference"
        );
        assert!(
            reloaded_a == reloaded_b,
            "{label}: CR 104.4b: printing-only reversals are one state after a reload"
        );

        let mut runner = GameRunner::from_state(reloaded_a);
        runner
            .act(GameAction::CancelCast)
            .expect("cancel after the reload");
        assert_cancelled_with_printing(&runner, morph, "SAAA");
    }
}

/// CR 733.1: a descriptor saved before the comparison list existed compares by its restoration
/// list, never equals a new descriptor, gains no comparison list through a payment or a
/// concession, and still cancels exactly.
#[test]
fn a_legacy_reversal_compares_by_its_restoration_list_and_keeps_no_comparison_list() {
    let (a, morph, lands) = face_down_under_tax(2, "SAAA");
    let (b, _, _) = face_down_under_tax(2, "SBBB");
    let new_descriptor = reload_raw(a.state());
    let legacy_a = legacy_of(a.state());
    let legacy_b = legacy_of(b.state());

    let saved = reversal(&legacy_a);
    assert!(
        saved.get("comparison_undo").is_none(),
        "reach: the legacy shape has no list"
    );
    assert_eq!(
        saved["undo"],
        reversal(a.state())["undo"],
        "reach: same restoration list"
    );
    assert!(
        legacy_a != new_descriptor,
        "a legacy descriptor never equals a new one"
    );
    assert!(new_descriptor != legacy_a, "and the relation is symmetric");
    assert!(legacy_a == legacy_a.clone());
    assert!(
        legacy_a == reload_raw(&legacy_a),
        "a legacy descriptor survives a raw reload"
    );
    assert!(
        reload_trusted(&legacy_a) == reload_trusted(&legacy_a),
        "and a trusted reload"
    );
    assert!(
        reversal(&reload_trusted(&legacy_a))
            .get("comparison_undo")
            .is_none(),
        "a reload never invents a list"
    );
    assert!(
        legacy_a != legacy_b,
        "legacy twins compare by their restoration lists, as before the list existed"
    );

    // A payment folds without gaining a comparison list.
    let mut runner = GameRunner::from_state(legacy_a.clone());
    tap(&mut runner, lands[0]);
    let folded = reversal(runner.state());
    assert_ne!(
        folded["undo"], saved["undo"],
        "reach: the fold rewrote the restoration list"
    );
    assert!(
        folded.get("comparison_undo").is_none(),
        "no list after a fold: {folded}"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the legacy cast");
    assert_cancelled_with_printing(&runner, morph, "SAAA");

    // A concession rebases without gaining one either.
    let (three, morph3, _) = face_down_under_tax(3, "SKAA");
    let mut runner = GameRunner::from_state(legacy_of(three.state()));
    let before = reversal(runner.state());
    runner
        .act(GameAction::Concede {
            player_id: PlayerId(2),
        })
        .expect("the third player concedes");
    let rebased = reversal(runner.state());
    assert_ne!(
        rebased["undo"], before["undo"],
        "reach: the rebase rewrote the list"
    );
    assert!(
        rebased.get("comparison_undo").is_none(),
        "no list after a rebase: {rebased}"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the legacy cast");
    assert!(runner.state().players[2].is_eliminated);
    assert_cancelled_with_printing(&runner, morph3, "SKAA");
}

fn load_dump() -> GameState {
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/dina_conqueror_4p.json.gz").as_slice(),
    )
    .read_to_string(&mut json)
    .expect("fixture .json.gz inflates to UTF-8 JSON");
    let envelope: Value = serde_json::from_str(&json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted snapshot satisfies the checked restore contract")
}

/// CR 203.1 + CR 206.1: the rules projection of a whole real `GameState` omits every printing.
/// This is the soundness constraint the comparison list rests on: nothing beneath `GameState`
/// serializes a printing through already-erased JSON, so projecting the typed state sees them all.
#[test]
fn whole_game_state_projection_omits_every_printing_on_a_real_board() {
    let mut state = load_dump();
    let objects = state.objects.len();
    assert!(objects > 0, "reach: the dump holds objects");
    assert_eq!(
        rules_projection(&state).expect("projects"),
        serde_json::to_value(&state).expect("serializes"),
        "reach: with no printing the projection is the wire form"
    );

    let ids: Vec<ObjectId> = state.objects.keys().copied().collect();
    for id in ids {
        stamp(&mut state, id, "SENT");
    }
    let wire = serde_json::to_value(&state).expect("serializes");
    assert_eq!(
        sentinels(&wire, "SENT"),
        2 * objects,
        "reach: the wire form holds every object's two printings"
    );
    let projected = rules_projection(&state).expect("projects");
    assert_eq!(
        sentinels(&projected, "SENT"),
        0,
        "the projection holds none"
    );
}
