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

use engine::game::scenario::{GameScenario, P0};
use engine::game::stack::resolve_next_with_limit;
use engine::game::triggers::ConsumedTriggerEventScope;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassRequest, CastPaymentMode, GameState, StackResolutionAutoPassOverlay,
    StackResolutionBudget, StackResolutionEntryFence, StackResolutionPolicy,
    StackResolutionSession,
};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use std::collections::{BTreeMap, BTreeSet};

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
