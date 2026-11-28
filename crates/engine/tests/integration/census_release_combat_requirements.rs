//! Census release witnesses for combat requirements: "all creatures able to
//! block ... do so", "must be blocked if able", "can block any number", and the
//! exert-as-attack observers that ride the same combat pipeline.
//!
//! Every card is the printed card loaded through `add_real_card` and driven
//! through public `GameRunner` actions. The only synthetic setup is a mana pool
//! (and, for the P1-turn observer pairs, the active/priority player, as in
//! `champions_of_minas_tirith.rs`). Block legality is read from the public
//! `combat::validate_blockers_for_player` (CR 509.1c) and from the engine-built
//! `DeclareBlockers` prompt.
//!
//! Witnesses: Alluring Scent, Bloodscent, Valor Made Real, Joraga Invocation,
//! Khenra Scrapper, Trueheart Twins, Vizier of the True, Taunting Arbormage,
//! The Masamune.

use std::collections::HashMap;

use engine::game::combat::{validate_blockers_for_player, AttackTarget, AttackerInfo, CombatState};
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::turns::execute_untap;
use engine::types::ability::{
    ContinuousModification, Effect, TargetFilter, TargetRef, TriggerCondition, TriggerEntry,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{StaticMode, TriggerCause};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn card(scenario: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(
        player,
        name,
        zone,
        shared_card_db().expect("printed fixture"),
    )
}

fn mana(units: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    units
        .iter()
        .flat_map(|&(kind, count)| vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); count])
        .collect()
}

// CR 508.1: declare attackers through the engine's own prompt.
fn declare_attacks(runner: &mut GameRunner, attacks: Vec<(ObjectId, AttackTarget)>) {
    drive_to_declare_attackers(runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(runner);
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn drive_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare blockers");
            }
            other => panic!("expected priority or declare blockers, got {other:?}"),
        }
    }
    panic!("did not reach declare blockers");
}

/// CR 509.1c: per blocker, the attackers whose must-be-blocked requirement
/// that single block would obey, read off the engine-built prompt.
fn must_be_blocked_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            must_be_blocked_targets,
            ..
        } => must_be_blocked_targets.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

fn valid_block_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            valid_block_targets,
            ..
        } => valid_block_targets.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// CR 509.1a + CR 101.1: how many attackers each able blocker may block; `None`
/// means any number.
fn block_capacities(runner: &GameRunner) -> HashMap<ObjectId, Option<u32>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            block_capacities, ..
        } => block_capacities.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// The parsed spell carries a resolution-installed static of the requested
/// mode (`GenericEffect` is where one-shot continuous requirements live).
fn spell_grants_static(object: &GameObject, wanted: impl Fn(&StaticMode) -> bool) -> bool {
    object.abilities.iter().any(|ability| {
        matches!(
            ability.effect.as_ref(),
            Effect::GenericEffect {
                static_abilities,
                ..
            } if static_abilities.iter().any(|definition| wanted(&definition.mode))
        )
    })
}

fn joraga_grants_pump_then_block_requirement(object: &GameObject) -> bool {
    object.abilities.iter().any(|ability| {
        matches!(ability.effect.as_ref(), Effect::PumpAll { .. })
            && ability.sub_ability.as_deref().is_some_and(|sub| {
                matches!(
                    sub.effect.as_ref(),
                    Effect::GenericEffect {
                        static_abilities,
                        ..
                    } if static_abilities
                        .iter()
                        .any(|definition| matches!(&definition.mode, StaticMode::MustBeBlocked { .. }))
                )
            })
    })
}

/// The printed `MustBeBlocked` static is the whole clause-1 witness: it must be
/// gated on the recipient attacking, and the equipment must also grant the
/// dying-creature trigger doubler.
fn masamune_has_expected_statics(object: &GameObject) -> bool {
    let mut attacking_requirement = false;
    let mut first_strike = false;
    let mut dying_doubler = false;
    for definition in object.static_definitions.iter_unchecked() {
        if matches!(&definition.mode, StaticMode::MustBeBlocked { by: None })
            && definition.condition.is_some()
        {
            attacking_requirement = true;
        }
        if matches!(&definition.mode, StaticMode::Continuous)
            && definition.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::FirstStrike
                    }
                )
            })
        {
            first_strike = true;
        }
        if definition.modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::GrantStaticAbility { definition }
                    if matches!(&definition.mode, StaticMode::DoubleTriggers {
                        cause: TriggerCause::CreatureDying
                    })
            )
        }) {
            dying_doubler = true;
        }
    }
    attacking_requirement && first_strike && dying_doubler
}

fn trigger_effect(entry: &TriggerEntry) -> Option<&Effect> {
    entry
        .definition
        .execute
        .as_deref()
        .map(|ability| ability.effect.as_ref())
}

fn arbormage_has_kicked_lure(object: &GameObject) -> bool {
    object.trigger_definitions.iter_unchecked().any(|entry| {
        entry.definition.mode == TriggerMode::ChangesZone
            && matches!(
                &entry.definition.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )
            && trigger_effect(entry).is_some_and(|effect| {
                matches!(
                    effect,
                    Effect::GenericEffect {
                        static_abilities,
                        ..
                    } if static_abilities.iter().any(|definition| matches!(
                        &definition.mode,
                        StaticMode::MustBeBlockedByAll { .. }
                    ))
                )
            })
    })
}

// CR 702.6a: the equip action announces, then chooses its attached creature.
// With a single legal creature the engine chooses it during the announcement,
// so the target prompt only appears when two or more creatures are legal.
fn equip(runner: &mut GameRunner, equipment: ObjectId, target: ObjectId) {
    let action = GameAction::Equip {
        equipment_id: equipment,
        target_id: target,
    };
    let before = runner.state().stack.len();
    runner.act(action.clone()).expect("announce equip");
    if matches!(runner.state().waiting_for, WaitingFor::EquipTarget { .. }) {
        runner.act(action).expect("choose equip target");
    }
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "the equip activation is the only new stack entry, got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(target)),
        "reach guard: the Equipment is attached"
    );
}

// CR 701.43a + CR 701.43d: the exert event names both the permanent and the
// player who chose to exert it.
fn exerted_events(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CreatureExerted {
                object_id,
                player: Some(player),
                ..
            } => Some((*object_id, *player)),
            _ => None,
        })
        .collect()
}

/// The transient `CantUntap` effect an exert installs on `id`.
fn cant_untap_markers(runner: &GameRunner, id: ObjectId) -> usize {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| effect.affected == TargetFilter::SpecificObject { id })
        .filter(|effect| {
            effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap
                    }
                )
            })
        })
        .count()
}

fn give_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn trigger_target_legal_targets(runner: &GameRunner, player: PlayerId) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player: actual,
            target_slots,
            ..
        } => {
            assert_eq!(*actual, player, "the observer's controller chooses");
            assert_eq!(target_slots.len(), 1, "one trigger target slot");
            target_slots[0].legal_targets.clone()
        }
        other => panic!("expected TriggerTargetSelection for {player:?}, got {other:?}"),
    }
}

fn select_trigger_target(runner: &mut GameRunner, target: ObjectId) {
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("choose the trigger target");
    runner.advance_until_stack_empty();
}

fn assert_no_trigger_target_prompt(runner: &GameRunner) {
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "no further trigger target prompt expected, got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// Alluring Scent / Bloodscent: "All creatures able to block target creature
// this turn do so." (CR 509.1c)
// ---------------------------------------------------------------------------

#[test]
fn alluring_scent_compels_every_able_blocker_to_block_the_target_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let lion = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let scent = card(&mut scenario, P0, "Alluring Scent", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 1), (ManaType::Green, 2)]));
    let mut runner = scenario.build();

    assert!(
        spell_grants_static(&runner.state().objects[&scent], |mode| matches!(
            mode,
            StaticMode::MustBeBlockedByAll { .. }
        )),
        "SHAPE: Alluring Scent must parse to a MustBeBlockedByAll grant: {:#?}",
        runner.state().objects[&scent].abilities
    );

    runner.cast(scent).target_object(target).resolve();
    declare_attacks(
        &mut runner,
        vec![
            (target, AttackTarget::Player(P1)),
            (lion, AttackTarget::Player(P1)),
        ],
    );

    let valid = valid_block_targets(&runner);
    for blocker in [b1, b2] {
        let legal = valid.get(&blocker).expect("able blocker is listed");
        assert!(
            legal.contains(&target) && legal.contains(&lion),
            "reach guard: both attackers are legal block targets"
        );
    }

    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(b1, vec![target]), (b2, vec![target])]),
        "CR 509.1c: every able blocker is compelled toward the targeted attacker only"
    );

    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "CR 509.1c: an empty declaration disobeys both requirements"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, target)]).is_err(),
        "CR 509.1c: one of two compelled blockers is not the maximum"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, lion), (b2, lion)]).is_err(),
        "blocking the untargeted attacker instead violates the lure"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, target), (b2, target)]).is_ok(),
        "CR 509.1c: both compelled blockers obey; the untargeted attacker may stay unblocked"
    );
}

#[test]
fn bloodscent_compels_every_able_blocker_to_block_the_target_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let bloodscent = card(&mut scenario, P0, "Bloodscent", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3), (ManaType::Green, 1)]));
    let mut runner = scenario.build();

    assert!(
        spell_grants_static(&runner.state().objects[&bloodscent], |mode| matches!(
            mode,
            StaticMode::MustBeBlockedByAll { .. }
        )),
        "SHAPE: Bloodscent must parse to a MustBeBlockedByAll grant: {:#?}",
        runner.state().objects[&bloodscent].abilities
    );

    runner.cast(bloodscent).target_object(target).resolve();
    declare_attacks(
        &mut runner,
        vec![
            (target, AttackTarget::Player(P1)),
            (bear, AttackTarget::Player(P1)),
        ],
    );

    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(b1, vec![target]), (b2, vec![target])]),
        "CR 509.1c: the lure names the targeted Savannah Lions only"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "CR 509.1c: an empty declaration disobeys the lure"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, bear), (b2, bear)]).is_err(),
        "blocking only the untargeted Grizzly Bears disobeys the lure"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, bear), (b2, target)]).is_err(),
        "CR 509.1c: each able blocker must obey its own requirement"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, target), (b2, target)]).is_ok(),
        "fully blocking the targeted Lions is legal; the untargeted Bear stays unblocked"
    );
}

// ---------------------------------------------------------------------------
// Valor Made Real: "Target creature can block any number of creatures this
// turn." (CR 509.1a + CR 101.1)
// ---------------------------------------------------------------------------

#[test]
fn valor_made_real_lets_the_target_block_any_number_of_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a1 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let a2 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let a3 = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let target = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let other = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let valor = card(&mut scenario, P0, "Valor Made Real", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::White, 1)]));
    let mut runner = scenario.build();

    assert!(
        spell_grants_static(&runner.state().objects[&valor], |mode| matches!(
            mode,
            StaticMode::ExtraBlockers { count: None }
        )),
        "SHAPE: Valor Made Real must parse to ExtraBlockers{{count: None}}: {:#?}",
        runner.state().objects[&valor].abilities
    );

    // CR 509.1a: baseline — without the spell, one creature blocks one attacker.
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![
            AttackerInfo::attacking_player(a1, P1),
            AttackerInfo::attacking_player(a2, P1),
            AttackerInfo::attacking_player(a3, P1),
        ],
        ..Default::default()
    });
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(target, a1)]).is_ok(),
        "reach guard: the target can block one attacker before the spell"
    );
    assert!(
        validate_blockers_for_player(
            runner.state(),
            P1,
            &[(target, a1), (target, a2), (target, a3)]
        )
        .is_err(),
        "CR 509.1a: without ExtraBlockers the default limit is one attacker"
    );
    runner.state_mut().combat = None;

    runner.cast(valor).target_object(target).resolve();
    declare_attacks(
        &mut runner,
        vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
            (a3, AttackTarget::Player(P1)),
        ],
    );

    let capacities = block_capacities(&runner);
    assert_eq!(
        capacities.get(&target),
        Some(&None),
        "CR 509.1a: 'any number' is an uncapped block capacity"
    );
    assert_eq!(
        capacities.get(&other),
        Some(&Some(1)),
        "reach guard: an untargeted blocker keeps the one-blocker default"
    );

    assert!(
        validate_blockers_for_player(
            runner.state(),
            P1,
            &[(target, a1), (target, a2), (target, a3)]
        )
        .is_ok(),
        "CR 509.1a + CR 101.1: the target may block all three attackers"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(other, a1), (other, a2)]).is_err(),
        "the untargeted blocker still may not block two attackers"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(other, a1)]).is_ok(),
        "reach guard: the untargeted blocker can block one attacker"
    );
}

// ---------------------------------------------------------------------------
// Joraga Invocation: "Each creature you control gets +3/+3 until end of turn
// and must be blocked this turn if able."
// ---------------------------------------------------------------------------

#[test]
fn joraga_invocation_pumps_each_creature_and_requires_the_attacking_ones_to_be_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a1 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let a2 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let home = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let joraga = card(&mut scenario, P0, "Joraga Invocation", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 4), (ManaType::Green, 2)]));
    let mut runner = scenario.build();

    assert!(
        joraga_grants_pump_then_block_requirement(&runner.state().objects[&joraga]),
        "SHAPE: Joraga Invocation must parse to PumpAll then a MustBeBlocked grant: {:#?}",
        runner.state().objects[&joraga].abilities
    );

    runner.cast(joraga).resolve();
    evaluate_layers(runner.state_mut());

    // CR 611.2c: the +3/+3 applies to every creature the controller had at
    // resolution; the nonattacker is pumped too.
    for id in [a1, a2] {
        assert_eq!(
            (
                runner.state().objects[&id].power,
                runner.state().objects[&id].toughness
            ),
            (Some(5), Some(5)),
            "each Grizzly Bears is 5/5"
        );
    }
    assert_eq!(
        (
            runner.state().objects[&home].power,
            runner.state().objects[&home].toughness
        ),
        (Some(5), Some(4)),
        "the nonattacking Savannah Lions is pumped (2/1 -> 5/4)"
    );
    for id in [b1, b2] {
        assert_eq!(
            (
                runner.state().objects[&id].power,
                runner.state().objects[&id].toughness
            ),
            (Some(2), Some(2)),
            "CR 611.2c: an opponent's creature is outside the affected set"
        );
    }

    declare_attacks(
        &mut runner,
        vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
        ],
    );

    let named = must_be_blocked_targets(&runner);
    for blocker in [b1, b2] {
        let attackers = named.get(&blocker).expect("able blocker is named");
        assert!(
            attackers.contains(&a1) && attackers.contains(&a2),
            "CR 509.1c: both attacking pumped creatures carry the requirement"
        );
    }

    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "CR 509.1c: neither requirement is obeyed"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, a1)]).is_err(),
        "CR 509.1c: obeying one of two requirements is not the maximum"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, a1), (b2, a2)]).is_ok(),
        "CR 509.1c: each attacker blocked by its own blocker is a valid declaration"
    );
}

#[test]
fn joraga_invocation_does_not_grant_its_locked_set_to_a_later_entrant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a1 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let a2 = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let joraga = card(&mut scenario, P0, "Joraga Invocation", Zone::Hand);
    let goblin = card(&mut scenario, P0, "Raging Goblin", Zone::Hand);
    scenario.with_mana_pool(
        P0,
        mana(&[
            (ManaType::Colorless, 4),
            (ManaType::Green, 2),
            (ManaType::Red, 1),
        ]),
    );
    let mut runner = scenario.build();

    assert!(
        joraga_grants_pump_then_block_requirement(&runner.state().objects[&joraga]),
        "SHAPE: Joraga Invocation must parse to PumpAll then a MustBeBlocked grant: {:#?}",
        runner.state().objects[&joraga].abilities
    );

    runner.cast(joraga).resolve();
    // CR 702.10a: the real Raging Goblin has haste, so it can attack on the
    // turn it enters.
    runner.cast(goblin).resolve();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&goblin].zone,
        Zone::Battlefield,
        "reach guard: the late creature entered after Joraga resolved"
    );
    assert_eq!(
        (
            runner.state().objects[&goblin].power,
            runner.state().objects[&goblin].toughness
        ),
        (Some(1), Some(1)),
        "CR 611.2c: a creature that entered after resolution is not pumped"
    );
    assert!(
        !active_static_definitions(runner.state(), &runner.state().objects[&goblin])
            .any(|definition| matches!(&definition.mode, StaticMode::MustBeBlocked { .. })),
        "CR 611.2c: the block requirement's affected set was fixed at resolution"
    );

    declare_attacks(
        &mut runner,
        vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
            (goblin, AttackTarget::Player(P1)),
        ],
    );

    let named = must_be_blocked_targets(&runner);
    for blocker in [b1, b2] {
        let attackers = named.get(&blocker).expect("able blocker is named");
        assert!(
            attackers.contains(&a1) && attackers.contains(&a2),
            "reach guard: the resolution-time attackers are still required"
        );
        assert!(
            !attackers.contains(&goblin),
            "CR 611.2c: the late entrant is not required to be blocked"
        );
    }
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "reach guard: the original affected set still creates requirements"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, a1), (b2, a2)]).is_ok(),
        "CR 611.2c: blocking the original attackers is complete; the late entrant may go unblocked"
    );
}

// ---------------------------------------------------------------------------
// Khenra Scrapper: exert-as-attack (CR 508.1g, CR 701.43a/d)
// ---------------------------------------------------------------------------

fn khenra_attacking(scenario: &mut GameScenario) -> (ObjectId, ObjectId) {
    let khenra = card(scenario, P0, "Khenra Scrapper", Zone::Battlefield);
    let blocker = card(scenario, P1, "Grizzly Bears", Zone::Battlefield);
    (khenra, blocker)
}

#[test]
fn khenra_scrapper_exert_pumps_and_keeps_it_tapped_through_its_next_untap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (khenra, _blocker) = khenra_attacking(&mut scenario);
    let mut runner = scenario.build();

    let object = &runner.state().objects[&khenra];
    assert!(
        object.trigger_definitions.iter_unchecked().any(|entry| {
            entry.definition.mode == TriggerMode::Exerted
                && trigger_effect(entry).is_some_and(|effect| matches!(effect, Effect::Pump { .. }))
        }),
        "SHAPE: Khenra Scrapper must parse to an exert-as-attack trigger that pumps: {:#?}",
        object.trigger_definitions
    );
    assert!(
        object.keywords.contains(&Keyword::Menace),
        "SHAPE: printed Menace"
    );

    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    runner
        .declare_attackers(&[(khenra, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(
        runner.waiting_for_kind(),
        "ExertChoice",
        "CR 508.1g: exert is offered as an optional attack cost"
    );

    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("choose to exert")
        .events;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(exerted_events(&events), vec![(khenra, P0)]);
    assert!(runner.state().exerted_this_turn.contains(&khenra));
    assert_eq!(
        runner.state().objects[&khenra].power,
        Some(4),
        "CR 701.43d: the linked trigger gives +2/+0"
    );
    assert_eq!(runner.state().objects[&khenra].base_power, Some(2));
    assert_eq!(cant_untap_markers(&runner, khenra), 1);
    assert!(
        runner.state().objects[&khenra]
            .keywords
            .contains(&Keyword::Menace),
        "the printed Menace remains after the layer pass"
    );

    // CR 701.43a: it does not untap during its controller's next untap step.
    runner.state_mut().active_player = P0;
    runner.state_mut().turn_number += 2;
    execute_untap(runner.state_mut(), &mut Vec::new());
    assert!(
        runner.state().objects[&khenra].tapped,
        "CR 701.43a: an exerted creature skips its next untap"
    );
    assert_eq!(
        cant_untap_markers(&runner, khenra),
        0,
        "CR 701.43a: the no-untap effect expires at that same untap step"
    );
}

#[test]
fn khenra_scrapper_declined_exert_leaves_it_unpumped_and_untaps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (khenra, _blocker) = khenra_attacking(&mut scenario);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(khenra, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");

    let events = runner
        .act(GameAction::ChooseExert { exert: false })
        .expect("decline the exert cost")
        .events;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert!(exerted_events(&events).is_empty());
    assert!(!runner.state().exerted_this_turn.contains(&khenra));
    assert_eq!(
        runner.state().objects[&khenra].power,
        Some(2),
        "CR 508.1g: declining pays no cost, so the pump never happens"
    );
    assert_eq!(cant_untap_markers(&runner, khenra), 0);

    runner.state_mut().active_player = P0;
    runner.state_mut().turn_number += 2;
    execute_untap(runner.state_mut(), &mut Vec::new());
    assert!(
        !runner.state().objects[&khenra].tapped,
        "an unexerted creature untaps normally"
    );
}

// ---------------------------------------------------------------------------
// Trueheart Twins: exert-as-attack plus the "whenever you exert a creature"
// observer (CR 701.43d)
// ---------------------------------------------------------------------------

#[test]
fn trueheart_twins_exert_pumps_its_controllers_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let twins = card(&mut scenario, P0, "Trueheart Twins", Zone::Battlefield);
    let bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();

    let object = &runner.state().objects[&twins];
    let observers: Vec<_> = object
        .trigger_definitions
        .iter_unchecked()
        .filter(|entry| {
            entry.definition.mode == TriggerMode::Exerted
                && entry.definition.valid_target == Some(TargetFilter::Controller)
                && trigger_effect(entry)
                    .is_some_and(|effect| matches!(effect, Effect::PumpAll { .. }))
        })
        .collect();
    assert_eq!(
        observers.len(),
        1,
        "SHAPE: exactly one 'whenever you exert a creature' PumpAll observer: {:#?}",
        object.trigger_definitions
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(twins, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");

    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("choose to exert")
        .events;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(exerted_events(&events), vec![(twins, P0)]);
    assert_eq!(
        runner.state().objects[&twins].power,
        Some(5),
        "4/4 plus +1/+0"
    );
    assert_eq!(
        runner.state().objects[&bear].power,
        Some(3),
        "CR 701.43d: each creature the controller controls gets +1/+0"
    );
}

#[test]
fn trueheart_twins_declined_exert_pumps_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let twins = card(&mut scenario, P0, "Trueheart Twins", Zone::Battlefield);
    let bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(twins, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");

    let events = runner
        .act(GameAction::ChooseExert { exert: false })
        .expect("decline")
        .events;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert!(exerted_events(&events).is_empty());
    assert_eq!(runner.state().objects[&twins].power, Some(4));
    assert_eq!(runner.state().objects[&bear].power, Some(2));
}

#[test]
fn trueheart_twins_observes_only_its_controllers_exerts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0_twins = card(&mut scenario, P0, "Trueheart Twins", Zone::Battlefield);
    let p0_bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let p1_twins = card(&mut scenario, P1, "Trueheart Twins", Zone::Battlefield);
    let p1_bear = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(p1_twins, AttackTarget::Player(P0))])
        .expect("P1 attacks P0");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");

    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("P1 exerts its own attacker")
        .events;
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        exerted_events(&events),
        vec![(p1_twins, P1)],
        "reach guard: the event happened and names P1"
    );
    assert_eq!(
        runner.state().objects[&p1_twins].power,
        Some(5),
        "P1's observer pumps P1's attacker"
    );
    assert_eq!(
        runner.state().objects[&p1_bear].power,
        Some(3),
        "P1's observer pumps P1's other creature"
    );
    assert_eq!(
        runner.state().objects[&p0_twins].power,
        Some(4),
        "CR 109.5: P0's observer did not trigger for P1's exert"
    );
    assert_eq!(
        runner.state().objects[&p0_bear].power,
        Some(2),
        "CR 109.5: 'you' is the observer's controller, so P0's set is untouched"
    );
}

// ---------------------------------------------------------------------------
// Vizier of the True: exert-as-attack plus "tap target creature an opponent
// controls" (CR 701.43d + CR 603.3d)
// ---------------------------------------------------------------------------

#[test]
fn vizier_of_the_true_exert_taps_an_opponents_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = card(&mut scenario, P0, "Vizier of the True", Zone::Battlefield);
    let p0_bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let p1_bear = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let p1_other = card(&mut scenario, P1, "Savannah Lions", Zone::Battlefield);
    let mut runner = scenario.build();

    let object = &runner.state().objects[&vizier];
    assert!(
        object.trigger_definitions.iter_unchecked().any(|entry| {
            entry.definition.mode == TriggerMode::Exerted
                && entry.definition.valid_target == Some(TargetFilter::Controller)
                && trigger_effect(entry)
                    .is_some_and(|effect| matches!(effect, Effect::SetTapState { .. }))
        }),
        "SHAPE: Vizier of the True must parse to a tapped-target observer: {:#?}",
        object.trigger_definitions
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(vizier, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("choose to exert");
    runner.advance_until_stack_empty();

    let legal = trigger_target_legal_targets(&runner, P0);
    assert!(
        legal.contains(&TargetRef::Object(p1_bear)) && legal.contains(&TargetRef::Object(p1_other)),
        "CR 115.1: the observer may tap any creature an opponent controls"
    );
    assert!(
        !legal.contains(&TargetRef::Object(p0_bear)),
        "the controller's own creatures are not legal targets"
    );
    select_trigger_target(&mut runner, p1_bear);
    assert_no_trigger_target_prompt(&runner);

    assert!(runner.state().objects[&p1_bear].tapped);
    assert!(!runner.state().objects[&p1_other].tapped);
    assert!(!runner.state().objects[&p0_bear].tapped);
}

#[test]
fn vizier_of_the_true_declined_exert_taps_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = card(&mut scenario, P0, "Vizier of the True", Zone::Battlefield);
    let p1_bear = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(vizier, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    runner
        .act(GameAction::ChooseExert { exert: false })
        .expect("decline");
    runner.advance_until_stack_empty();

    assert_no_trigger_target_prompt(&runner);
    assert!(!runner.state().objects[&p1_bear].tapped);
}

#[test]
fn vizier_of_the_true_observes_only_its_controllers_exerts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _ = card(&mut scenario, P0, "Vizier of the True", Zone::Battlefield);
    let p0_bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let p1_vizier = card(&mut scenario, P1, "Vizier of the True", Zone::Battlefield);
    let p1_bear = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(p1_vizier, AttackTarget::Player(P0))])
        .expect("P1 attacks P0");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("P1 exerts its own attacker");
    runner.advance_until_stack_empty();

    let legal = trigger_target_legal_targets(&runner, P1);
    assert!(
        legal.contains(&TargetRef::Object(p0_bear)),
        "reach guard: P1's observer may tap P0's creature"
    );
    assert!(
        !legal.contains(&TargetRef::Object(p1_bear)),
        "P1's observer cannot tap P1's own creature"
    );
    select_trigger_target(&mut runner, p0_bear);
    assert_no_trigger_target_prompt(&runner);

    assert!(
        runner.state().objects[&p0_bear].tapped,
        "P1's observer tapped P0's creature"
    );
    assert!(
        !runner.state().objects[&p1_bear].tapped,
        "P1's observer targeted P0's creature, not P1's own"
    );
}

// ---------------------------------------------------------------------------
// Taunting Arbormage: kicked ETB lure (CR 702.33a + CR 603.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum KickerPayment {
    Pay,
    Decline,
}

fn cast_arbormage(
    runner: &mut GameRunner,
    spell: ObjectId,
    payment: KickerPayment,
) -> Vec<&'static str> {
    let action = GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    runner
        .act(action)
        .expect("Taunting Arbormage should be castable from the funded pool");
    let mut prompts = Vec::new();
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                prompts.push("OptionalCostChoice");
                let index = match payment {
                    KickerPayment::Pay => 0,
                    KickerPayment::Decline => 1,
                };
                runner
                    .act(GameAction::DecideOptionalCost { index })
                    .expect("answer the kicker payment");
            }
            // CR 601.2h: finalize the auto-payment if the engine surfaces it.
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize mana payment");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected cast prompt {other:?}"),
        }
    }
    prompts
}

fn resolve_kicked_arbormage_etb(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "reach guard: the kicked creature resolved"
    );
    let legal = trigger_target_legal_targets(runner, P0);
    assert!(
        legal.contains(&TargetRef::Object(target)),
        "CR 603.4: the kicked intervening-if passed and the trigger targets"
    );
    select_trigger_target(runner, target);
    assert_no_trigger_target_prompt(runner);
}

fn resolve_unkicked_arbormage_etb(runner: &mut GameRunner, spell: ObjectId) {
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "reach guard: the unkicked creature resolved"
    );
    assert_no_trigger_target_prompt(runner);
}

#[test]
fn taunting_arbormage_kicked_etb_compels_every_able_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let lion = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let arbormage = card(&mut scenario, P0, "Taunting Arbormage", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 5), (ManaType::Green, 1)]));
    let mut runner = scenario.build();

    assert!(
        arbormage_has_kicked_lure(&runner.state().objects[&arbormage]),
        "SHAPE: a kicked-gated ETB MustBeBlockedByAll trigger: {:#?}",
        runner.state().objects[&arbormage].trigger_definitions
    );

    let prompts = cast_arbormage(&mut runner, arbormage, KickerPayment::Pay);
    assert!(
        prompts.contains(&"OptionalCostChoice"),
        "CR 702.33a: the kicker was offered"
    );
    resolve_kicked_arbormage_etb(&mut runner, arbormage, target);

    declare_attacks(
        &mut runner,
        vec![
            (target, AttackTarget::Player(P1)),
            (lion, AttackTarget::Player(P1)),
        ],
    );

    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(b1, vec![target]), (b2, vec![target])]),
        "CR 509.1c: the kicked ETB lure compels every able blocker"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "CR 509.1c: an empty declaration disobeys the lure"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, target)]).is_err(),
        "CR 509.1c: one of two compelled blockers is not the maximum"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, lion), (b2, lion)]).is_err(),
        "the untargeted attacker is not the lure's target"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, target), (b2, target)]).is_ok(),
        "CR 509.1c: both compelled blockers obey; the untargeted attacker stays unblocked"
    );
}

#[test]
fn taunting_arbormage_unkicked_etb_is_a_silent_no_op() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let lion = card(&mut scenario, P0, "Savannah Lions", Zone::Battlefield);
    let b1 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let b2 = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let arbormage = card(&mut scenario, P0, "Taunting Arbormage", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 5), (ManaType::Green, 1)]));
    let mut runner = scenario.build();

    assert!(
        arbormage_has_kicked_lure(&runner.state().objects[&arbormage]),
        "SHAPE: the parsed trigger exists independently of the kicker payment: {:#?}",
        runner.state().objects[&arbormage].trigger_definitions
    );

    let prompts = cast_arbormage(&mut runner, arbormage, KickerPayment::Decline);
    assert!(
        prompts.contains(&"OptionalCostChoice"),
        "CR 702.33a: the kicker was offered and declined"
    );
    resolve_unkicked_arbormage_etb(&mut runner, arbormage);

    declare_attacks(
        &mut runner,
        vec![
            (target, AttackTarget::Player(P1)),
            (lion, AttackTarget::Player(P1)),
        ],
    );

    assert!(
        must_be_blocked_targets(&runner).is_empty(),
        "CR 603.4: the intervening-if was false, so no lure exists"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_ok(),
        "CR 603.4: with no requirement, an empty declaration is legal"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(b1, lion), (b2, lion)]).is_ok(),
        "nothing compels a block toward either attacker"
    );
}

// ---------------------------------------------------------------------------
// The Masamune: equipped attacking creature has first strike and must be
// blocked; the equipped creature's creature-dying triggers double.
// ---------------------------------------------------------------------------

#[test]
fn the_masamune_equipped_attacker_has_first_strike_and_must_be_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artist = card(&mut scenario, P0, "Blood Artist", Zone::Battlefield);
    let masamune = card(&mut scenario, P0, "The Masamune", Zone::Battlefield);
    let blocker = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 2)]));
    let mut runner = scenario.build();

    assert!(
        masamune_has_expected_statics(&runner.state().objects[&masamune]),
        "SHAPE: The Masamune must carry the attacking-gated first-strike and \
         must-be-blocked statics plus the dying doubler: {:#?}",
        runner.state().objects[&masamune].static_definitions
    );

    equip(&mut runner, masamune, artist);
    evaluate_layers(runner.state_mut());
    declare_attacks(&mut runner, vec![(artist, AttackTarget::Player(P1))]);
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&artist]
            .keywords
            .contains(&Keyword::FirstStrike),
        "CR 702.7a: the attacking equipped creature has first strike"
    );
    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(blocker, vec![artist])]),
        "CR 509.1c: the attacking equipped creature must be blocked if able"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_err(),
        "CR 509.1c: an able blocker must block the equipped attacker"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(blocker, artist)]).is_ok(),
        "blocking the equipped attacker obeys the requirement"
    );
}

#[test]
fn the_masamune_equipped_attacker_has_neither_when_not_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artist = card(&mut scenario, P0, "Blood Artist", Zone::Battlefield);
    let other = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let masamune = card(&mut scenario, P0, "The Masamune", Zone::Battlefield);
    let _ = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 2)]));
    let mut runner = scenario.build();

    equip(&mut runner, masamune, artist);
    evaluate_layers(runner.state_mut());
    assert!(
        !runner.state().objects[&artist]
            .keywords
            .contains(&Keyword::FirstStrike),
        "the 'as long as equipped creature is attacking' condition is false at \
         precombat main"
    );

    // Reach guard: another creature attacks, so the blocker prompt genuinely
    // exists; the equipped but nonattacking Artist is simply not a requirement.
    declare_attacks(&mut runner, vec![(other, AttackTarget::Player(P1))]);
    assert!(
        must_be_blocked_targets(&runner).is_empty(),
        "CR 509.1c: the nonattacking equipped creature carries no requirement"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_ok(),
        "an empty declaration is legal while nothing must be blocked"
    );
    assert!(
        !runner.state().objects[&artist]
            .keywords
            .contains(&Keyword::FirstStrike),
        "the nonattacking creature still has no first strike"
    );
}

#[test]
fn the_masamune_unequipped_attacker_has_neither_keyword_nor_requirement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artist = card(&mut scenario, P0, "Blood Artist", Zone::Battlefield);
    let masamune = card(&mut scenario, P0, "The Masamune", Zone::Battlefield);
    let blocker = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let mut runner = scenario.build();

    assert!(
        masamune_has_expected_statics(&runner.state().objects[&masamune]),
        "reach guard: the unattached Equipment parsed its statics: {:#?}",
        runner.state().objects[&masamune].static_definitions
    );

    declare_attacks(&mut runner, vec![(artist, AttackTarget::Player(P1))]);

    assert!(
        !runner.state().objects[&artist]
            .keywords
            .contains(&Keyword::FirstStrike),
        "an unequipped creature does not get the equipment's first strike"
    );
    assert!(
        must_be_blocked_targets(&runner).is_empty(),
        "the unattached Equipment's EquippedBy static affects nothing"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[]).is_ok(),
        "reach guard: the able blocker ({blocker:?}) is not compelled"
    );
}

#[test]
fn the_masamune_doubles_equipped_blood_artists_creature_death_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artist = card(&mut scenario, P0, "Blood Artist", Zone::Battlefield);
    let masamune = card(&mut scenario, P0, "The Masamune", Zone::Battlefield);
    let victim = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let murder = card(&mut scenario, P0, "Murder", Zone::Hand);
    // Equip {2} + Murder {1}{B}{B}.
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3), (ManaType::Black, 2)]));
    let mut runner = scenario.build();

    assert!(
        masamune_has_expected_statics(&runner.state().objects[&masamune]),
        "SHAPE: The Masamune must parse the dying-creature doubler: {:#?}",
        runner.state().objects[&masamune].static_definitions
    );
    assert!(
        !runner.state().objects[&artist]
            .trigger_definitions
            .is_empty(),
        "SHAPE: Blood Artist's death observer exists"
    );

    equip(&mut runner, masamune, artist);
    evaluate_layers(runner.state_mut());

    // CR 603.2d: the dying creature is controlled by P1, not the doubler's
    // controller; both Blood Artist triggers still fire and each chooses P1.
    let outcome = runner
        .cast(murder)
        .target_object(victim)
        .target_player(P1)
        .resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 2);
}

#[test]
fn the_masamune_does_not_double_a_non_death_trigger_on_the_equipped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let twins = card(&mut scenario, P0, "Trueheart Twins", Zone::Battlefield);
    let bear = card(&mut scenario, P0, "Grizzly Bears", Zone::Battlefield);
    let masamune = card(&mut scenario, P0, "The Masamune", Zone::Battlefield);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 2)]));
    let mut runner = scenario.build();

    assert!(
        masamune_has_expected_statics(&runner.state().objects[&masamune]),
        "SHAPE: The Masamune must parse the dying-creature cause predicate: {:#?}",
        runner.state().objects[&masamune].static_definitions
    );

    equip(&mut runner, masamune, twins);
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(twins, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("choose to exert");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    // CR 603.2d: the doubler's cause is a creature dying; an exert event is not
    // that cause, so the observer resolves exactly once (+1/+0, not +2/+0).
    assert_eq!(
        runner.state().objects[&twins].power,
        Some(5),
        "the equipped observer's non-death trigger must not double"
    );
    assert_eq!(
        runner.state().objects[&bear].power,
        Some(3),
        "the pump is applied once"
    );
}

#[test]
fn the_masamune_absent_does_not_double_blood_artists_death_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artist = card(&mut scenario, P0, "Blood Artist", Zone::Battlefield);
    let victim = card(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let murder = card(&mut scenario, P0, "Murder", Zone::Hand);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 1), (ManaType::Black, 2)]));
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&artist]
            .trigger_definitions
            .is_empty(),
        "SHAPE: Blood Artist's death observer exists without The Masamune"
    );

    let outcome = runner
        .cast(murder)
        .target_object(victim)
        .target_player(P1)
        .resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P0, 1);
}
