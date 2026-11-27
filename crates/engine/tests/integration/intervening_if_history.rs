//! Whole-card runtime witness for a turn-history intervening-if:
//!
//! * Syrix, Carrier of the Flame — "At the beginning of each end step, if a
//!   creature card left your graveyard this turn, target Phoenix you control
//!   deals damage equal to its power to any target."
//!
//! The witness uses the printed card through `add_real_card`, drives real
//! player actions, asserts every prompt it meets, and ends in a settled state.
//! The negative siblings pair the same observable with a positive control run
//! so a missing trigger cannot pass for the wrong reason.
//!
//! CR references (verified against the Comprehensive Rules):
//!   - CR 603.4: an intervening "if" is checked when the trigger event occurs
//!     and again as the ability resolves.
//!   - CR 404.1: a player's graveyard is their discard pile; cards move there
//!     from any zone.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect(
        "integration_cards.json.gz must contain the printed witnesses; a missing fixture is a \
         failure, never a skip",
    )
}

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

fn pool(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count).map(|_| mana(color)).collect()
}

fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
}

fn assert_settled(runner: &GameRunner, player: engine::types::player::PlayerId, context: &str) {
    assert!(
        runner.state().stack.is_empty(),
        "{context}: the stack must be empty; stack = {:?}",
        runner.stack_names()
    );
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::Priority { player: current } if *current == player
        ),
        "{context}: expected Priority for {player:?}; got {:?}",
        runner.state().waiting_for
    );
}

fn zone_departures_from(runner: &GameRunner, zone: Zone) -> Vec<(ObjectId, Zone)> {
    runner
        .state()
        .zone_changes_this_turn
        .iter()
        .filter(|record| record.from_zone == Some(zone))
        .map(|record| (record.object_id, record.to_zone))
        .collect()
}

// ---------------------------------------------------------------------------
// Syrix, Carrier of the Flame
// ---------------------------------------------------------------------------

struct SyrixBoard {
    runner: GameRunner,
    syrix: ObjectId,
    p0_creature_card: ObjectId,
    p0_noncreature_card: ObjectId,
    p1_creature_card: ObjectId,
    raise_dead: ObjectId,
    regrowth: ObjectId,
    reanimate: ObjectId,
}

fn syrix_board(db: &CardDatabase) -> SyrixBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let syrix = scenario.add_real_card(P0, "Syrix, Carrier of the Flame", Zone::Battlefield, db);
    let p0_creature_card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let p0_noncreature_card = scenario.add_real_card(P0, "Lightning Bolt", Zone::Graveyard, db);
    let p1_creature_card = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    let raise_dead = scenario.add_real_card(P0, "Raise Dead", Zone::Hand, db);
    let regrowth = scenario.add_real_card(P0, "Regrowth", Zone::Hand, db);
    let reanimate = scenario.add_real_card(P0, "Reanimate", Zone::Hand, db);
    // Raise Dead {B}, Regrowth {1}{G}, Reanimate {B}.
    let mut pool_units = pool(ManaType::Black, 2);
    pool_units.extend(pool(ManaType::Green, 2));
    scenario.with_mana_pool(P0, pool_units);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().phase = Phase::PostCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(
        runner.state().objects[&syrix].power,
        Some(3),
        "precondition: Syrix is a 3/3"
    );
    assert!(
        zone_departures_from(&runner, Zone::Graveyard).is_empty(),
        "precondition: no card has left a graveyard yet this turn"
    );

    SyrixBoard {
        runner,
        syrix,
        p0_creature_card,
        p0_noncreature_card,
        p1_creature_card,
        raise_dead,
        regrowth,
        reanimate,
    }
}

/// Pass priority until either a trigger target prompt or a stable priority
/// window appears. A panic covers any other prompt, because neither Syrix path
/// produces one.
fn pass_to_syrix_prompt(runner: &mut GameRunner) {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority to resolve Syrix's trigger must succeed");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected waiting state at Syrix's end step: {other:?}"),
        }
    }
    panic!("the end-step machinery did not reach a stable state for Syrix");
}

/// Positive control run: a creature card leaves P0's graveyard via Raise Dead,
/// the end-step trigger fires, both target slots are asserted and answered, and
/// Syrix deals 3 to P1. Shared by the negative siblings as their reach guard.
fn resolve_syrix_creature_return(db: &CardDatabase) -> GameRunner {
    let mut board = syrix_board(db);

    board
        .runner
        .cast(board.raise_dead)
        .target_object(board.p0_creature_card)
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.p0_creature_card].zone,
        Zone::Hand,
        "precondition: Raise Dead must return the creature card to P0's hand"
    );
    let departures = zone_departures_from(&board.runner, Zone::Graveyard);
    assert!(
        departures
            .iter()
            .any(|(id, _)| *id == board.p0_creature_card),
        "reach guard: the turn history must record the creature card leaving P0's graveyard; \
         departures = {departures:?}"
    );

    board.runner.advance_to_end_step();
    pass_to_syrix_prompt(&mut board.runner);

    let WaitingFor::TriggerTargetSelection {
        player,
        trigger_controller,
        trigger_event: _trigger_event,
        trigger_events: _trigger_events,
        target_slots,
        mode_labels: _mode_labels,
        target_constraints: _target_constraints,
        selection,
        source_id,
        description: _description,
    } = board.runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 603.4: a creature card left P0's graveyard, so Syrix must ask for its two \
             targets; got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Syrix's controller chooses its targets");
    assert_eq!(trigger_controller, Some(P0));
    assert_eq!(source_id, Some(board.syrix));
    assert_eq!(
        target_slots.len(),
        2,
        "Syrix has two target slots: the Phoenix source and the damage recipient"
    );
    assert_eq!(
        selection.current_slot, 0,
        "the written order puts the Phoenix source first"
    );
    assert_eq!(
        target_slots[0].legal_targets,
        vec![TargetRef::Object(board.syrix)],
        "the first slot is 'target Phoenix you control': Syrix is the only Phoenix"
    );
    // 'any target': both players and Syrix, the only creature on the battlefield.
    let any_target = &target_slots[1].legal_targets;
    assert_eq!(
        any_target.len(),
        3,
        "any target = two players + Syrix, got {any_target:?}"
    );
    for target in [
        TargetRef::Player(P0),
        TargetRef::Player(P1),
        TargetRef::Object(board.syrix),
    ] {
        assert!(
            any_target.contains(&target),
            "{target:?} must be legal, got {any_target:?}"
        );
    }

    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.syrix)),
        })
        .expect("Syrix must be a legal Phoenix source");

    let WaitingFor::TriggerTargetSelection {
        player,
        trigger_controller: _trigger_controller,
        trigger_event: _trigger_event,
        trigger_events: _trigger_events,
        target_slots,
        mode_labels: _mode_labels,
        target_constraints: _target_constraints,
        selection,
        source_id: _source_id,
        description: _description,
    } = board.runner.state().waiting_for.clone()
    else {
        panic!(
            "after the source target, the second damage-recipient slot must open; got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(selection.current_slot, 1);
    assert!(
        target_slots[1]
            .legal_targets
            .contains(&TargetRef::Player(P1)),
        "P1 must be a legal 'any target' recipient; legal targets = {:?}",
        target_slots[1].legal_targets
    );

    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 must be a legal damage recipient");

    settle(&mut board.runner);
    assert_settled(&board.runner, P0, "Syrix after its trigger resolved");
    board.runner
}

/// Syrix, Carrier of the Flame positive: "At the beginning of each end step, if
/// a creature card left your graveyard this turn, target Phoenix you control
/// deals damage equal to its power to any target." (CR 603.4.)
#[test]
fn syrix_carrier_of_the_flame_creature_card_leaving_graveyard_deals_power_damage() {
    let db = card_db();
    let runner = resolve_syrix_creature_return(db);

    assert_eq!(
        runner.life(P1),
        17,
        "Syrix's power is 3, so the chosen any-target (P1) must lose exactly 3 life"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "P0 must not lose life from its own trigger"
    );
}

/// Syrix negative: a NONCREATURE card leaving P0's graveyard does not satisfy
/// "a creature card left your graveyard this turn" (CR 603.4).
///
/// Reach guards: the control run with a creature card departing reaches the
/// trigger and deals 3; the witness's own turn history records the Lightning
/// Bolt leaving P0's graveyard.
#[test]
fn syrix_carrier_of_the_flame_noncreature_card_leaving_graveyard_does_not_trigger() {
    let db = card_db();

    let control = resolve_syrix_creature_return(db);
    assert_eq!(
        control.life(P1),
        17,
        "reach guard: the creature-card control run must make Syrix deal 3"
    );

    let mut board = syrix_board(db);
    board
        .runner
        .cast(board.regrowth)
        .target_object(board.p0_noncreature_card)
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.p0_noncreature_card].zone,
        Zone::Hand,
        "Regrowth must return the noncreature card to P0's hand"
    );
    let departures = zone_departures_from(&board.runner, Zone::Graveyard);
    assert!(
        departures
            .iter()
            .any(|(id, _)| *id == board.p0_noncreature_card),
        "reach guard: the noncreature card really left P0's graveyard; departures = {departures:?}"
    );

    board.runner.advance_to_end_step();
    pass_to_syrix_prompt(&mut board.runner);

    assert!(
        matches!(
            &board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "CR 603.4: a noncreature card leaving the graveyard must not put Syrix's trigger on \
         the stack; got {:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(board.runner.life(P1), 20, "P1 must not take Syrix damage");
    assert_settled(&board.runner, P0, "Syrix after a noncreature card left");
}

/// Syrix negative: a creature card leaving the OPPONENT's graveyard does not
/// satisfy "left YOUR graveyard" (CR 603.4).
///
/// Reach guards: the control run with P0's own creature card reaches the
/// trigger and deals 3; the witness's turn history records the P1-owned
/// creature card leaving P1's graveyard and entering under P0's control.
#[test]
fn syrix_carrier_of_the_flame_opponent_graveyard_departure_does_not_trigger() {
    let db = card_db();

    let control = resolve_syrix_creature_return(db);
    assert_eq!(
        control.life(P1),
        17,
        "reach guard: the own-graveyard control run must make Syrix deal 3"
    );

    let mut board = syrix_board(db);
    board
        .runner
        .cast(board.reanimate)
        .target_object(board.p1_creature_card)
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.p1_creature_card].zone,
        Zone::Battlefield,
        "Reanimate must put the opponent's creature card onto the battlefield"
    );
    assert_eq!(
        board.runner.state().objects[&board.p1_creature_card].controller,
        P0,
        "Reanimate puts the creature under P0's control"
    );
    let departure = board
        .runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| {
            record.object_id == board.p1_creature_card && record.from_zone == Some(Zone::Graveyard)
        })
        .expect("reach guard: P1's creature card must have left P1's graveyard");
    assert_eq!(
        departure.owner, P1,
        "reach guard: the departing card belongs to P1, not to Syrix's controller"
    );

    board.runner.advance_to_end_step();
    pass_to_syrix_prompt(&mut board.runner);

    assert!(
        matches!(
            &board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "CR 603.4: an opponent's graveyard departure must not trigger 'left your graveyard'; \
         got {:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(board.runner.life(P1), 20, "P1 must not take Syrix damage");
    assert_settled(&board.runner, P0, "Syrix after an opponent's card left");
}
