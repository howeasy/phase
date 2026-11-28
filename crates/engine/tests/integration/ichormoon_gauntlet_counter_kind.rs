//! Ichormoon Gauntlet (ONE):
//!
//! "Planeswalkers you control have "[0]: Proliferate" and "[−12]: Take an
//! extra turn after this one."
//! Whenever you cast a noncreature spell, choose a counter on target permanent.
//! Put an additional counter of that kind on that permanent."
//!
//! Witness for the cast trigger — the same "choose a counter on target ..."
//! clause Dathon, His Heart Open and Clockspinning already exercise. CR 603.3d:
//! the trigger's target permanent is chosen when the trigger is put on the
//! stack (above the spell that cast it). CR 608.2d + CR 122.1 / CR 122.6: at
//! resolution the controller picks one kind among those already on the target
//! and exactly one more counter of that kind is put on it. Both kinds' final
//! counts are asserted, so an implementation that ignores the kind answer — or
//! grows the wrong kind — cannot pass.
//!
//! The planeswalker grant is witnessed on the same footing: the granted
//! "[0]: Proliferate" runs through the real loyalty-activation path and opens
//! the CR 701.34a choice, and the granted "[−12]" pays twelve loyalty for an
//! extra turn (CR 500.7) — both read from the live object after the layer-6
//! ability grant (CR 613.1f), not from the parsed fixture text.

use crate::support::shared_card_db;
use engine::game::coverage::card_face_gaps;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, AbilityDefinition, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// Reach guard, mirroring the Dathon file: the Gauntlet's cast trigger must
/// parse with no `Unimplemented` residue anywhere in its effect chain.
fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

/// P0 controls Ichormoon Gauntlet and a blank enchantment filler carrying one
/// Stun and one Shield counter (added through the scenario's counter API, as
/// the Dathon file does), and holds a real Lightning Bolt with its printed {R}
/// already in the pool.
fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gauntlet = scenario.add_real_card(P0, "Ichormoon Gauntlet", Zone::Battlefield, db);
    let carrier = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    scenario.with_counter(carrier, CounterType::Stun, 1);
    scenario.with_counter(carrier, CounterType::Shield, 1);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    // Lightning Bolt's printed {R}: auto-payment draws it from the pool
    // (spell_cost_is_payable_from_pool -> CastPaymentMode::Auto, no stop).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let runner = scenario.build();
    let triggers = runner.state().objects[&gauntlet]
        .trigger_definitions
        .as_slice();
    assert!(
        triggers.iter().any(|entry| entry.definition.execute.is_some())
            && !triggers.iter().any(|entry| entry
                .definition
                .execute
                .as_deref()
                .is_some_and(contains_unimplemented)),
        "Ichormoon Gauntlet's cast trigger must parse with an effect and no Unimplemented residue: {triggers:?}"
    );
    (runner, gauntlet, carrier, bolt)
}

/// Pass priority until the stack drains after the counter-kind answer.
fn settle(runner: &mut GameRunner) {
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
}

/// CR 601.2 + CR 603.3d + CR 608.2d + CR 122.6: casting the noncreature
/// Lightning Bolt puts the Gauntlet's trigger on the stack with a
/// target-permanent prompt for P0; resolving it asks P0 for the counter KIND
/// (exactly one option per kind already on the target); only the chosen kind
/// grows by exactly one. The trigger sits above the Bolt, so the kind prompt
/// must open before the Bolt can resolve — a driver that drains the stack
/// without the prompt fails the loop's empty-stack guard.
#[test]
fn noncreature_cast_prompts_kind_and_only_chosen_kind_grows() {
    let (mut runner, gauntlet, carrier, bolt) = setup();
    let p1_life_before = runner.life(P1);

    // CR 601.2 + CR 603.3d: real production cast of Lightning Bolt aimed at P1.
    // The cast also declares the Gauntlet's trigger target: the helper matches
    // the carrier to the trigger's target slot and the player to Bolt's.
    runner
        .cast(bolt)
        .target_player(P1)
        .target_objects(&[carrier])
        .commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the Bolt and the Gauntlet's trigger are on the stack"
    );

    // The trigger is on top of the Bolt; passing priority resolves it and its
    // counter-kind prompt must open. If the stack drains instead, the
    // implementation skipped the kind choice and this guard fires.
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "the Gauntlet trigger resolved without a counter-kind prompt"
                );
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("expected the counter-kind prompt, got {other:?}"),
        }
    }
    let WaitingFor::NamedChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Gauntlet trigger must ask for the counter kind, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 chooses the counter kind");
    assert_eq!(
        options.len(),
        2,
        "exactly one option per kind already on the carrier: {options:?}"
    );
    assert!(
        options.iter().any(|o| o.eq_ignore_ascii_case("stun"))
            && options.iter().any(|o| o.eq_ignore_ascii_case("shield")),
        "the offered kinds must be exactly Stun and Shield: {options:?}"
    );
    let choice = options
        .iter()
        .find(|o| o.eq_ignore_ascii_case("shield"))
        .cloned()
        .expect("Shield is an option");
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("answer the counter-kind choice");
    settle(&mut runner);

    // CR 122.6: exactly one more counter of the CHOSEN kind. Both counts are
    // asserted, so choosing (or applying) Stun instead fails here.
    assert_eq!(
        counters(&runner, carrier, CounterType::Shield),
        2,
        "the chosen Shield kind grew by exactly one"
    );
    assert_eq!(
        counters(&runner, carrier, CounterType::Stun),
        1,
        "the unchosen Stun kind is unchanged"
    );
    // The noncreature spell itself then resolved: CR 120.3, Bolt's 3 damage to P1.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "Lightning Bolt resolved on P1 after the trigger"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&gauntlet].zone,
        Zone::Battlefield,
        "the Gauntlet itself is untouched"
    );
}

// ─── Loyalty grants through the real activation pipeline ────────────────────

/// P0's precombat main with (optionally) Ichormoon Gauntlet, a blank
/// planeswalker carrying `loyalty` loyalty counters, and a filler permanent
/// with one Stun counter. Layers are flushed so the Gauntlet's layer-6
/// `GrantAbility` (CR 613.1f) has been applied to the planeswalker's live
/// `abilities` when the test reads them.
fn loyalty_board(loyalty: u32, with_gauntlet: bool) -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_gauntlet {
        scenario.add_real_card(P0, "Ichormoon Gauntlet", Zone::Battlefield, db);
    }
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Ichormoon Test Walker", "Jace", loyalty, "")
        .id();
    let carrier = scenario
        .add_enchantment_from_oracle(P0, "Test Counter Carrier", "")
        .id();
    scenario.with_counter(carrier, CounterType::Stun, 1);
    let mut runner = scenario.build();
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    (runner, walker, carrier)
}

/// Index of the live loyalty ability with the given loyalty cost on `source`.
fn loyalty_index(runner: &GameRunner, source: ObjectId, amount: i32) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost == Some(AbilityCost::Loyalty { amount }))
        .unwrap_or_else(|| {
            panic!(
                "expected a [{amount}] loyalty ability on the planeswalker; got {:?}",
                runner.state().objects[&source].abilities
            )
        })
}

/// CR 606.3 + CR 606.4 + CR 613.1f + CR 701.34a: the Gauntlet's "[0]:
/// Proliferate" exists on P0's planeswalker only because the grant was applied,
/// activating it through the loyalty path removes no loyalty, and resolution
/// asks P0 which permanents grow — exactly one counter of the kind already
/// there lands on the chosen permanent.
#[test]
fn granted_zero_ability_proliferates_through_loyalty_activation() {
    let (mut runner, walker, carrier) = loyalty_board(12, true);
    let zero = loyalty_index(&runner, walker, 0);
    assert!(
        matches!(
            runner.state().objects[&walker].abilities[zero]
                .effect
                .as_ref(),
            Effect::Proliferate
        ),
        "the [0] grant must be Proliferate; got {:?}",
        runner.state().objects[&walker].abilities[zero].effect
    );

    // CR 606.2 + CR 606.3: own main phase, empty stack, priority — the real
    // loyalty activation route. It stops at the proliferate choice.
    let _outcome = runner.activate(walker, zero).resolve();

    let WaitingFor::ProliferateChoice { player, eligible } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 701.34a: proliferate must ask which permanents grow, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 chooses what to proliferate");
    assert!(
        eligible.contains(&TargetRef::Object(carrier)),
        "the carrier with a Stun counter is eligible: {eligible:?}"
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(carrier)],
        })
        .expect("the proliferate answer must be accepted");

    assert_eq!(
        counters(&runner, carrier, CounterType::Stun),
        2,
        "the chosen permanent gained exactly one counter of its kind"
    );
    assert_eq!(
        runner.state().objects[&walker].loyalty,
        Some(12),
        "CR 606.4: a [0] cost removes no loyalty"
    );
    assert!(runner.state().stack.is_empty());
}

/// CR 500.7 + CR 606.3 + CR 606.4 + CR 613.1f: the granted "[−12]: Take an
/// extra turn after this one." is a real loyalty ability on the planeswalker;
/// activating it once (the CR 606.3 limit) pays twelve of its twenty loyalty
/// and, on resolution, queues the extra turn for its controller.
#[test]
fn granted_minus_twelve_ability_buys_an_extra_turn() {
    let (mut runner, walker, _carrier) = loyalty_board(20, true);
    let minus_twelve = loyalty_index(&runner, walker, -12);
    let ability = &runner.state().objects[&walker].abilities[minus_twelve];
    assert_eq!(
        ability.cost,
        Some(AbilityCost::Loyalty { amount: -12 }),
        "the granted cost is [-12] loyalty"
    );
    assert!(
        matches!(ability.effect.as_ref(), Effect::ExtraTurn { .. }),
        "the granted effect is an extra turn; got {:?}",
        ability.effect
    );

    let _outcome = runner.activate(walker, minus_twelve).resolve();

    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "CR 500.7: exactly one extra turn is queued"
    );
    assert_eq!(
        runner.state().extra_turns[0].player,
        P0,
        "the activator takes the extra turn"
    );
    assert_eq!(
        runner.state().objects[&walker].loyalty,
        Some(8),
        "CR 606.4: [-12] removes twelve loyalty (20 -> 8)"
    );
    assert_eq!(
        runner.state().objects[&walker].zone,
        Zone::Battlefield,
        "the walker survives the payment"
    );
    assert!(runner.state().stack.is_empty());
}

/// Negative control: the identical blank planeswalker without the Gauntlet has
/// no live loyalty abilities, so neither grant is offered (the same board and
/// the same layer flush).
#[test]
fn without_the_gauntlet_the_walker_gains_no_loyalty_abilities() {
    let (runner, walker, _carrier) = loyalty_board(20, false);
    assert!(
        runner.state().objects[&walker].abilities.is_empty(),
        "no Gauntlet means no granted loyalty abilities: {:?}",
        runner.state().objects[&walker].abilities
    );
    assert!(
        !engine::game::planeswalker::can_activate_loyalty(runner.state(), walker, P0),
        "the bare planeswalker offers no loyalty activation"
    );
}

/// Whole-card coverage: the planeswalker loyalty grants and the cast trigger
/// both parse, so Ichormoon Gauntlet reports no gap. Read from the
/// production-written fixture entry; the witness above proves the trigger's
/// behavior.
#[test]
fn ichormoon_gauntlet_whole_card_has_no_coverage_gap() {
    let db = shared_card_db().expect("integration card fixture must load");
    let face = db
        .get_face_by_name("Ichormoon Gauntlet")
        .expect("Ichormoon Gauntlet is in the integration fixture");
    assert!(
        !face.triggers.is_empty() && !face.static_abilities.is_empty(),
        "REACH: the grant static and the cast trigger parse: {face:?}"
    );
    assert!(
        card_face_gaps(face).is_empty(),
        "Ichormoon Gauntlet must have no coverage gaps: {:?}",
        card_face_gaps(face)
    );
}
