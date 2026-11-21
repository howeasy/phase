//! Whole-card runtime witnesses for the turn-history intervening-if family:
//!
//! * Sarevok, Deathbringer — "At the beginning of each player's end step, if no
//!   permanents left the battlefield this turn, that player loses X life, where
//!   X is Sarevok's power."
//! * Syrix, Carrier of the Flame — "At the beginning of each end step, if a
//!   creature card left your graveyard this turn, target Phoenix you control
//!   deals damage equal to its power to any target."
//! * Sandstalker Moloch — "When this creature enters, if an opponent cast a
//!   blue and/or black spell this turn, look at the top four cards of your
//!   library. You may reveal a permanent card from among them and put it into
//!   your hand. Put the rest on the bottom of your library in a random order."
//!
//! Each witness uses the printed card through `add_real_card`, drives real
//! player actions, asserts every prompt it meets, and ends in a settled state.
//! The negative siblings pair the same observable with a positive control run
//! so a missing trigger cannot pass for the wrong reason.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.4: an intervening "if" is checked when the trigger event occurs
//!     and again as the ability resolves.
//!   - CR 404.1: a player's graveyard is their discard pile; cards move there
//!     from any zone.
//!   - CR 105.1: the five colors; "blue and/or black" is the either-color
//!     disjunction.
//!   - CR 702.8a: flash means the card may be cast any time an instant could.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
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

fn colors_of_cast_spells(
    runner: &GameRunner,
    player: engine::types::player::PlayerId,
) -> Vec<ManaColor> {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&player)
        .map(|records| {
            records
                .iter()
                .flat_map(|record| record.colors.iter().copied())
                .collect()
        })
        .unwrap_or_default()
}

fn cast_count(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&player)
        .map_or(0, |records| records.len())
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
// Sarevok, Deathbringer
// ---------------------------------------------------------------------------

/// Sarevok, Deathbringer: "At the beginning of each player's end step, if no
/// permanents left the battlefield this turn, that player loses X life, where X
/// is Sarevok's power." (CR 603.4.)
///
/// Quiet turn: nothing leaves the battlefield, so at each player's end step the
/// player whose step it is loses life equal to Sarevok's printed power (3).
#[test]
fn sarevok_deathbringer_quiet_end_step_charges_active_player_only() {
    let db = card_db();

    // P0's end step: only P0 loses 3.
    let p0_run = quiet_sarevok_end_step(db, P0);
    assert_eq!(
        p0_run.life(P0),
        17,
        "CR 603.4: on P0's quiet end step Sarevok's controller is the active player; \
         P0 must lose exactly Sarevok's power (3)"
    );
    assert_eq!(
        p0_run.life(P1),
        20,
        "P1 must not lose life on P0's end step"
    );
    assert_settled(&p0_run, P0, "Sarevok on P0's end step");

    // P1's end step: only P1 loses 3.
    let p1_run = quiet_sarevok_end_step(db, P1);
    assert_eq!(
        p1_run.life(P1),
        17,
        "CR 603.4: 'each player's end step' charges the player whose end step it is, \
         so P1 loses exactly 3 on P1's own end step"
    );
    assert_eq!(
        p1_run.life(P0),
        20,
        "P0 must not lose life on P1's end step"
    );
    assert_settled(&p1_run, P1, "Sarevok on P1's end step");
}

/// Sarevok, Deathbringer negative sibling: during the turn a printed removal
/// spell destroys a permanent, so the intervening-if "no permanents left the
/// battlefield this turn" is false and the end-step trigger does not fire
/// (CR 603.4).
///
/// Reach guards in this same test: (1) the destroyed creature is really in the
/// graveyard and the turn history records its Battlefield -> Graveyard
/// departure; (2) a quiet control run of the same scenario reaches the
/// end-step trigger and charges the active player 3.
#[test]
fn sarevok_deathbringer_permanent_leaving_battlefield_suppresses_end_step_trigger() {
    let db = card_db();

    // Control: the same board with no departure loses 3 at the end step.
    let control = quiet_sarevok_end_step(db, P0);
    assert_eq!(
        control.life(P0),
        17,
        "reach guard: the quiet control run must reach Sarevok's end-step trigger"
    );
    assert_settled(&control, P0, "Sarevok quiet control");

    // Witness: P0's Lightning Bolt kills P1's Grizzly Bears during P0's turn.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let sarevok = scenario.add_real_card(P0, "Sarevok, Deathbringer", Zone::Battlefield, db);
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(ManaType::Red, 1));
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().phase = Phase::PostCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(
        runner.state().objects[&sarevok].power,
        Some(3),
        "precondition: Sarevok, Deathbringer is a 3/4"
    );

    runner.cast(bolt).target_object(victim).resolve();
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach guard: the removal spell must actually destroy the permanent"
    );
    let departures = zone_departures_from(&runner, Zone::Battlefield);
    assert_eq!(
        departures,
        vec![(victim, Zone::Graveyard)],
        "reach guard: exactly the destroyed permanent must have left the battlefield this turn"
    );

    runner.advance_to_end_step();
    settle(&mut runner);

    assert_eq!(
        runner.life(P0),
        20,
        "CR 603.4: the permanent left the battlefield this turn, so P0 must not lose life"
    );
    assert_eq!(runner.life(P1), 20, "P1 must not lose life either");
    assert_settled(&runner, P0, "Sarevok after a permanent left");
}

/// Build the quiet-turn board and drive to the end step of `active`'s turn.
fn quiet_sarevok_end_step(
    db: &CardDatabase,
    active: engine::types::player::PlayerId,
) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let sarevok = scenario.add_real_card(P0, "Sarevok, Deathbringer", Zone::Battlefield, db);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().phase = Phase::PostCombatMain;
    runner.state_mut().active_player = active;
    runner.state_mut().priority_player = active;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: active };

    assert_eq!(
        runner.state().objects[&sarevok].power,
        Some(3),
        "precondition: Sarevok, Deathbringer is a 3/4"
    );
    assert!(
        zone_departures_from(&runner, Zone::Battlefield).is_empty(),
        "precondition: the quiet turn must start with no permanent departures"
    );

    runner.advance_to_end_step();
    settle(&mut runner);
    runner
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

// ---------------------------------------------------------------------------
// Sandstalker Moloch
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SandstalkerPrecedingSpell {
    OpponentBlue,
    OpponentBlack,
    OpponentGreen,
    ControllerBlue,
}

impl SandstalkerPrecedingSpell {
    fn caster(self) -> engine::types::player::PlayerId {
        match self {
            SandstalkerPrecedingSpell::OpponentBlue
            | SandstalkerPrecedingSpell::OpponentBlack
            | SandstalkerPrecedingSpell::OpponentGreen => P1,
            SandstalkerPrecedingSpell::ControllerBlue => P0,
        }
    }
}

struct SandstalkerRun {
    runner: GameRunner,
    sandstalker: ObjectId,
    top_four: [ObjectId; 4],
    bottom_card: ObjectId,
    p1_grizzly: ObjectId,
}

/// Build the board, have the designated player cast a printed spell of the
/// designated color, then have P0 cast Sandstalker Moloch with flash (CR
/// 702.8a) so it enters this turn. Returns with whatever prompt the entry
/// raised; callers assert it.
fn run_sandstalker(db: &CardDatabase, spell: SandstalkerPrecedingSpell) -> SandstalkerRun {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sandstalker = scenario.add_real_card(P0, "Sandstalker Moloch", Zone::Hand, db);
    // P0's library top -> bottom. The first four are the dig pile: permanent,
    // nonpermanent, permanent, nonpermanent; the fifth proves the rest is
    // bottomed (not merely moved into the top positions).
    let grizzly = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let cancel = scenario.add_real_card(P0, "Cancel", Zone::Library, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let p1_grizzly = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let opt = scenario.add_real_card(P0, "Opt", Zone::Hand, db);
    let dark_ritual = scenario.add_real_card(P1, "Dark Ritual", Zone::Hand, db);
    let giant_growth = scenario.add_real_card(P1, "Giant Growth", Zone::Hand, db);
    let p1_opt = scenario.add_real_card(P1, "Opt", Zone::Hand, db);
    // P1 needs a library for Opt's draw.
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);

    // P0: {1}{G}{G} for Sandstalker, {U} for the controller-blue Opt.
    let mut p0_pool = pool(ManaType::Green, 3);
    p0_pool.extend(pool(ManaType::Blue, 2));
    scenario.with_mana_pool(P0, p0_pool);
    // P1: one mana of each color for the chosen preceding spell.
    let mut p1_pool = pool(ManaType::Blue, 1);
    p1_pool.extend(pool(ManaType::Black, 1));
    p1_pool.extend(pool(ManaType::Green, 1));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Resolve the preceding spell from the designated player.
    runner.state_mut().phase = Phase::PreCombatMain;
    runner.state_mut().active_player = spell.caster();
    runner.state_mut().priority_player = spell.caster();
    runner.state_mut().waiting_for = WaitingFor::Priority {
        player: spell.caster(),
    };

    match spell {
        SandstalkerPrecedingSpell::OpponentBlue => {
            let outcome = runner.cast(p1_opt).resolve();
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
                "OpponentBlue: Opt must resolve to a priority window; got {:?}",
                outcome.final_waiting_for()
            );
        }
        SandstalkerPrecedingSpell::OpponentBlack => {
            let outcome = runner.cast(dark_ritual).resolve();
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
                "OpponentBlack: Dark Ritual must resolve to a priority window; got {:?}",
                outcome.final_waiting_for()
            );
        }
        SandstalkerPrecedingSpell::OpponentGreen => {
            let outcome = runner
                .cast(giant_growth)
                .target_object(p1_grizzly)
                .resolve();
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
                "OpponentGreen: Giant Growth must resolve to a priority window; got {:?}",
                outcome.final_waiting_for()
            );
        }
        SandstalkerPrecedingSpell::ControllerBlue => {
            let outcome = runner.cast(opt).resolve();
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
                "ControllerBlue: Opt must resolve to a priority window; got {:?}",
                outcome.final_waiting_for()
            );
        }
    }

    let preceding_spell = match spell {
        SandstalkerPrecedingSpell::OpponentBlue => p1_opt,
        SandstalkerPrecedingSpell::OpponentBlack => dark_ritual,
        SandstalkerPrecedingSpell::OpponentGreen => giant_growth,
        SandstalkerPrecedingSpell::ControllerBlue => opt,
    };
    assert_eq!(
        runner.state().objects[&preceding_spell].zone,
        Zone::Graveyard,
        "reach guard: the preceding spell must have been cast and resolved"
    );

    // P0 flash-casts Sandstalker Moloch (CR 702.8a) now, on the same turn.
    pass_priority_to(&mut runner, P0);
    runner.cast(sandstalker).resolve();

    assert_eq!(
        runner.state().objects[&sandstalker].zone,
        Zone::Battlefield,
        "reach guard: Sandstalker Moloch must enter the battlefield this turn"
    );

    SandstalkerRun {
        runner,
        sandstalker,
        top_four: [grizzly, bolt, island, cancel],
        bottom_card: forest,
        p1_grizzly,
    }
}

fn pass_priority_to(runner: &mut GameRunner, target: engine::types::player::PlayerId) {
    for _ in 0..4 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { player } if *player != target => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("cannot hand priority to {target:?} from {other:?}"),
        }
    }
    panic!("priority never reached {target:?}");
}

/// Assert the full dig prompt for the top four, keep one permanent, and verify
/// the rest go to the bottom of the library. Returns the settled runner.
fn assert_dig_prompt_and_keep(run: &mut SandstalkerRun, chosen: ObjectId) {
    let WaitingFor::DigChoice {
        player,
        library_owner,
        cards,
        keep_count,
        up_to,
        selectable_cards,
        kept_destination,
        rest_destination,
        rest_remainder: _rest_remainder,
        rest_split_top_count: _rest_split_top_count,
        rest_order: _rest_order,
        source_id,
        enter_tapped: _enter_tapped,
        enters_attacking: _enters_attacking,
    } = run.runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 603.4: the condition is true, so the enters trigger must ask P0 to dig; got {:?}",
            run.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 looks at the top four and chooses");
    assert_eq!(library_owner, P0);
    assert_eq!(
        cards,
        run.top_four.to_vec(),
        "Sandstalker must look at exactly the top four cards"
    );
    assert_eq!(
        selectable_cards,
        vec![run.top_four[0], run.top_four[2]],
        "only the two permanent cards among the top four are selectable \
         (Grizzly Bears and Island)"
    );
    assert_eq!(keep_count, 1, "reveal at most one permanent card");
    assert!(
        up_to,
        "'You may reveal a permanent card' allows none or one"
    );
    assert_eq!(kept_destination, Some(Zone::Hand));
    assert_eq!(rest_destination, Some(Zone::Library));
    assert_eq!(source_id, Some(run.sandstalker));

    run.runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("keeping a legal permanent card must be accepted");
    settle(&mut run.runner);

    assert_eq!(
        run.runner.state().objects[&chosen].zone,
        Zone::Hand,
        "the revealed permanent card must reach P0's hand"
    );
    let library: Vec<ObjectId> = run
        .runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library.len(),
        4,
        "the remaining library must be the un-dug card plus the three rest cards"
    );
    assert_eq!(
        library[0], run.bottom_card,
        "the rest pile goes BELOW the card that was already under the top four"
    );
    let mut expected_rest: Vec<ObjectId> = run
        .top_four
        .iter()
        .copied()
        .filter(|id| *id != chosen)
        .collect();
    expected_rest.sort();
    let mut actual_rest = library[1..].to_vec();
    actual_rest.sort();
    assert_eq!(
        actual_rest, expected_rest,
        "the three unchosen cards must occupy the bottom positions below the card that was \
         already under the top four"
    );
    assert!(
        !library.contains(&chosen),
        "the kept card must not remain in the library"
    );
}

/// Sandstalker Moloch positive (blue): "When this creature enters, if an
/// opponent cast a blue and/or black spell this turn, look at the top four
/// cards of your library..." (CR 603.4 + CR 105.1.)
#[test]
fn sandstalker_moloch_opponent_blue_spell_enables_enters_dig() {
    let db = card_db();
    let mut run = run_sandstalker(db, SandstalkerPrecedingSpell::OpponentBlue);

    assert_eq!(
        colors_of_cast_spells(&run.runner, P1),
        vec![ManaColor::Blue],
        "reach guard: P1's only spell this turn is blue"
    );
    assert_eq!(
        cast_count(&run.runner, P0),
        1,
        "reach guard: P0's only cast is Sandstalker Moloch itself"
    );

    let chosen = run.top_four[0]; // Grizzly Bears
    assert_dig_prompt_and_keep(&mut run, chosen);
    assert_settled(&run.runner, P1, "Sandstalker after the blue-triggered dig");
}

/// Sandstalker Moloch positive (black): the "and/or" color disjunction also
/// accepts a black spell (CR 105.1).
#[test]
fn sandstalker_moloch_opponent_black_spell_enables_enters_dig() {
    let db = card_db();
    let mut run = run_sandstalker(db, SandstalkerPrecedingSpell::OpponentBlack);

    assert_eq!(
        colors_of_cast_spells(&run.runner, P1),
        vec![ManaColor::Black],
        "reach guard: P1's only spell this turn is black"
    );

    let chosen = run.top_four[2]; // Island
    assert_dig_prompt_and_keep(&mut run, chosen);
    assert_settled(&run.runner, P1, "Sandstalker after the black-triggered dig");
}

/// Sandstalker Moloch negative: a green (neither blue nor black) spell from the
/// opponent does not satisfy the intervening-if (CR 603.4 + CR 105.1).
///
/// Reach guards: a blue-spell control run of the identical board surfaces the
/// dig prompt and completes it; the witness's history holds exactly one green
/// spell cast by P1 and none by P0.
#[test]
fn sandstalker_moloch_opponent_green_spell_does_not_trigger() {
    let db = card_db();

    // Control: identical board, but the opponent's spell is blue.
    let mut control = run_sandstalker(db, SandstalkerPrecedingSpell::OpponentBlue);
    assert!(
        matches!(
            &control.runner.state().waiting_for,
            WaitingFor::DigChoice { .. }
        ),
        "reach guard: the blue control must reach the dig prompt; got {:?}",
        control.runner.state().waiting_for
    );
    let control_chosen = control.top_four[0];
    assert_dig_prompt_and_keep(&mut control, control_chosen);

    // Witness: same board, but P1 cast Giant Growth.
    let run = run_sandstalker(db, SandstalkerPrecedingSpell::OpponentGreen);
    assert_eq!(
        colors_of_cast_spells(&run.runner, P1),
        vec![ManaColor::Green],
        "reach guard: P1's only spell this turn is green"
    );
    assert_eq!(
        cast_count(&run.runner, P0),
        1,
        "reach guard: P0's only cast is Sandstalker Moloch itself"
    );
    assert_eq!(
        run.runner.state().objects[&run.p1_grizzly].zone,
        Zone::Battlefield,
        "reach guard: the green spell's target is still on the battlefield"
    );

    assert!(
        matches!(&run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 603.4: green is neither blue nor black, so no dig prompt may appear; got {:?}",
        run.runner.state().waiting_for
    );
    assert_settled(
        &run.runner,
        P1,
        "Sandstalker after an opponent's green spell",
    );
    let library: Vec<ObjectId> = run
        .runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        run.top_four
            .iter()
            .copied()
            .chain(std::iter::once(run.bottom_card))
            .collect::<Vec<_>>(),
        "no dig means the top four and the card below them stay in place"
    );
}

/// Sandstalker Moloch negative: the controller (not an opponent) casting the
/// blue spell does not satisfy "an opponent cast a blue and/or black spell this
/// turn" (CR 603.4).
///
/// Reach guards: a blue-spell-by-P1 control run of the identical board surfaces
/// and completes the dig prompt; the witness's history shows P0 cast the blue
/// spell and P1 cast none.
#[test]
fn sandstalker_moloch_controller_blue_spell_does_not_trigger() {
    let db = card_db();

    let mut control = run_sandstalker(db, SandstalkerPrecedingSpell::OpponentBlue);
    assert!(
        matches!(
            &control.runner.state().waiting_for,
            WaitingFor::DigChoice { .. }
        ),
        "reach guard: the opponent-blue control must reach the dig prompt; got {:?}",
        control.runner.state().waiting_for
    );
    let control_chosen = control.top_four[0];
    assert_dig_prompt_and_keep(&mut control, control_chosen);

    let run = run_sandstalker(db, SandstalkerPrecedingSpell::ControllerBlue);
    let p0_colors = colors_of_cast_spells(&run.runner, P0);
    assert!(
        p0_colors.contains(&ManaColor::Blue),
        "reach guard: P0 is the player who cast the blue spell; colors = {p0_colors:?}"
    );
    assert_eq!(
        cast_count(&run.runner, P0),
        2,
        "reach guard: P0 cast Opt and Sandstalker Moloch itself"
    );
    assert_eq!(
        cast_count(&run.runner, P1),
        0,
        "reach guard: P1 (the opponent of Sandstalker's controller) cast nothing"
    );

    assert!(
        matches!(&run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 603.4: the trigger needs an OPPONENT's blue/black spell, so no dig may appear; \
         got {:?}",
        run.runner.state().waiting_for
    );
    assert_settled(
        &run.runner,
        P0,
        "Sandstalker after its controller's blue spell",
    );
}
