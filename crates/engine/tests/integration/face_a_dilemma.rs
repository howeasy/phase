//! "Whenever you face a dilemma" (TRK). CR 700.17 (edition effective
//! 2026-11-06; the repo's pinned edition predates it): "A player faces a
//! dilemma whenever they choose one or more modes for a spell or ability." The
//! mode choices it reads are CR
//! 700.2a (spells and activated abilities, CR 601.2b / CR 602.2b), CR 700.2b
//! (triggered abilities, CR 603.3c) and CR 700.2e (a player other than the
//! controller chooses).
//!
//! One dilemma is faced per mode-choice announcement, not per mode. The trigger
//! event happens as the modes are announced (CR 603.2); the per-turn history is
//! written once the announcement reaches the stack (CR 601.2i + CR 733.1: an
//! undone announcement records nothing). Copies (CR 707.10, CR 700.2g) face
//! no dilemma (a copy copies the chosen modes and chooses none, CR 700.2g). Modes drawn at
//! random are still the controller's choice (CR 700.2b), so a nonempty random
//! draw faces one.
//!
//! Every named card below is the printed card, added from the curated card
//! database through `add_real_card`; its printed cost is paid with basic lands.
//! Textless fillers (vanilla creatures, library cards, basic lands) remain
//! scenario builders.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// `player`'s faced dilemmas in the per-turn history.
fn dilemmas(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(actor, kind)| *actor == player && *kind == PlayerActionKind::FaceDilemma)
        .count()
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// P0's precombat main phase, five library cards per seat.
fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..5 {
            scenario.add_spell_to_library_top(
                player,
                &format!("Library {}-{index}", player.0),
                false,
            );
        }
    }
    scenario
}

fn add_seven(scenario: &mut GameScenario, player: PlayerId, db: &CardDatabase) -> ObjectId {
    scenario.add_real_card(player, "Seven of Nine", Zone::Battlefield, db)
}

/// Abzan Charm in P0's hand, with its printed {W}{B}{G} payable.
fn add_abzan_charm(scenario: &mut GameScenario, db: &CardDatabase) -> ObjectId {
    for color in [ManaColor::White, ManaColor::Black, ManaColor::Green] {
        scenario.add_basic_land(P0, color);
    }
    scenario.add_real_card(P0, "Abzan Charm", Zone::Hand, db)
}

/// Prompts answered by `drive`, consumed in order; anything unplanned panics.
#[derive(Default)]
struct Plan {
    modes: Vec<Vec<usize>>,
    targets: Vec<TargetRef>,
    optional: Vec<bool>,
}

/// The actor of each mode and optional prompt `drive` answered.
#[derive(Debug, Default)]
struct Record {
    mode_choosers: Vec<PlayerId>,
    optional_actors: Vec<PlayerId>,
    /// `(player, choices)` of each `PayCost` prompt, answered with every choice.
    cost_choices: Vec<(PlayerId, Vec<ObjectId>)>,
}

/// Answer every prompt from `plan` until the stack is empty at priority.
fn drive(runner: &mut GameRunner, plan: Plan) -> Record {
    let Plan {
        modes,
        targets,
        optional,
    } = plan;
    let mut modes = modes.into_iter();
    let mut targets = targets.into_iter();
    let mut optional = optional.into_iter();
    let mut record = Record::default();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { player, .. }
            | WaitingFor::AbilityModeChoice { player, .. } => {
                record.mode_choosers.push(player);
                let indices = modes.next().expect("unplanned mode prompt");
                runner
                    .act(GameAction::SelectModes { indices })
                    .expect("choose the planned modes");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let target = targets.next().expect("unplanned target prompt");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choose the planned target");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                record.optional_actors.push(player);
                let accept = optional.next().expect("unplanned optional prompt");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the optional prompt");
            }
            WaitingFor::ScryChoice { cards, .. } | WaitingFor::SurveilChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keep the cards on top");
            }
            WaitingFor::PayCost {
                player, choices, ..
            } => {
                record.cost_choices.push((player, choices.clone()));
                runner
                    .act(GameAction::SelectCards { cards: choices })
                    .expect("pay the cost with its only choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return record;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the stack resolves");
            }
            other => panic!("unexpected prompt {other:?}; record {record:?}"),
        }
    }
    panic!("the stack did not resolve: {record:?}");
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the spell");
}

/// R1 — CR 601.2b + CR 700.2a: casting a modal spell faces one dilemma; Seven
/// of Nine draws once and the history records one dilemma.
#[test]
fn spell_mode_choice_faces_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, charm);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    // The charm left; it drew two; Seven drew one.
    assert_eq!(hand_len(&runner, P0), hand - 1 + 2 + 1);
    assert_eq!(runner.life(P0), life - 2, "REACH: the charm resolved");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R2 — CR 700.2a + CR 700.2d: two modes chosen are one dilemma.
#[test]
fn two_modes_are_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let melkotian = scenario.add_real_card(P0, "Melkotian Adjudicator", Zone::Battlefield, db);
    // Atarka's Command {R}{G}.
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Green);
    let command = scenario.add_real_card(P0, "Atarka's Command", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1_life = runner.life(P1);
    cast(&mut runner, command);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1, 3]],
            ..Plan::default()
        },
    );
    assert_eq!(runner.life(P1), p1_life - 3, "REACH: the command resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly one"
    );
    assert_eq!(plus_counters(&runner, melkotian), 1);
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R4a — CR 602.2b + CR 700.2a: an activated ability's mode choice faces a
/// dilemma; the history is written when the ability reaches the stack.
#[test]
fn activated_mode_choice_faces_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let cankerbloom = scenario.add_real_card(P0, "Cankerbloom", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cankerbloom");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![2]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    assert_eq!(
        runner.state().objects[&cankerbloom].zone,
        Zone::Graveyard,
        "REACH: the sacrifice cost was paid"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven drew one");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R4c — CR 603.2 + CR 602.2b: the dilemma is faced at the mode choice, before
/// the cost sacrifices Seven of Nine (P0's only artifact, so the sacrifice
/// prompt offers only Seven), and Seven's trigger still fires: Seven was on
/// the battlefield when the event happened.
#[test]
fn trigger_source_sacrificed_as_the_cost_still_triggers() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let seven = add_seven(&mut scenario, P0, db);
    let lobelia = scenario.add_real_card(P0, "Lobelia, Defender of Bag End", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let lives = (runner.life(P0), runner.life(P1));
    runner
        .act(GameAction::ActivateAbility {
            source_id: lobelia,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Lobelia");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.cost_choices, vec![(P0, vec![seven])]);
    assert_eq!(runner.state().objects[&seven].zone, Zone::Graveyard);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (lives.0 + 2, lives.1 - 2),
        "REACH: the mode resolved"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven's draw resolved");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R5 — CR 603.3c + CR 700.2b: a modal triggered ability's mode choice faces a
/// dilemma.
#[test]
fn triggered_mode_choice_faces_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // Charming Prince {1}{W}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let prince = scenario.add_real_card(P0, "Charming Prince", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, prince);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    assert_eq!(runner.life(P0), life + 3, "REACH: the mode resolved");
    assert_eq!(hand_len(&runner, P0), hand - 1 + 1, "Seven drew one");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R7 + R15 — "whenever YOU face a dilemma": P0's choice fires P0's Elim
/// Garak and Obsidian Order Operative, never P1's copies.
#[test]
fn only_the_choosers_dilemma_abilities_trigger() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    scenario.add_real_card(P0, "Elim Garak, Simple Tailor", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Obsidian Order Operative", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Elim Garak, Simple Tailor", Zone::Battlefield, db);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let (p0, p1) = (runner.life(P0), runner.life(P1));
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    // Charm: P0 −2. Garak: P1 −2, P0 +2. Operative: P1 −1, P0 +1. P1's Garak
    // does not fire, so P0 loses nothing more.
    assert_eq!(runner.life(P0), p0 - 2 + 2 + 1);
    assert_eq!(runner.life(P1), p1 - 2 - 1);
    assert_eq!(dilemmas(&runner, P0), 1);
    assert_eq!(dilemmas(&runner, P1), 0);
}

/// R8 — CR 700.2e: "An opponent chooses one" — the opponent who chooses faces
/// the dilemma, not the caster.
#[test]
fn the_mode_chooser_faces_the_dilemma_not_the_caster() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    scenario.add_real_card(P1, "Obsidian Order Operative", Zone::Battlefield, db);
    // Fatal Lore {2}{B}{B}.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let lore = scenario.add_real_card(P0, "Fatal Lore", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let (p0, p1) = (runner.life(P0), runner.life(P1));
    cast(&mut runner, lore);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![0]],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P1]);
    assert_eq!(hand_len(&runner, P0), hand - 1 + 3, "only the mode drew");
    assert_eq!((runner.life(P0), runner.life(P1)), (p0 - 1, p1 + 1));
    assert_eq!(dilemmas(&runner, P0), 0);
    assert_eq!(dilemmas(&runner, P1), 1);
}

/// R9 — CR 707.10 + CR 700.2g: a copy copies the modes and faces no dilemma.
#[test]
fn a_copy_faces_no_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // Atarka's Command {R}{G}, Twincast {U}{U}.
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Green);
    let command = scenario.add_real_card(P0, "Atarka's Command", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let twincast = scenario.add_real_card(P0, "Twincast", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1 = runner.life(P1);
    cast(&mut runner, command);
    runner
        .act(GameAction::SelectModes {
            indices: vec![1, 3],
        })
        .expect("choose the command's modes");
    cast(&mut runner, twincast);
    drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Object(command)],
            optional: vec![false],
            ..Plan::default()
        },
    );
    assert_eq!(runner.life(P1), p1 - 6, "REACH: the copy resolved");
    assert_eq!(hand_len(&runner, P0), hand - 2 + 1, "Seven drew once");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R10 — CR 733.1: a cast cancelled after the mode choice faces no dilemma
/// that is recorded, and its trigger never reaches the stack.
#[test]
fn a_cancelled_cast_records_no_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // Two legal exile targets, so the target prompt is raised.
    scenario.add_creature(P1, "P1 Giant", 4, 4);
    scenario.add_creature(P1, "P1 Second Giant", 4, 4);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, charm);
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choose the exile mode");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "REACH: the exile mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::CancelCast).expect("cancel the cast");
    drive(&mut runner, Plan::default());
    assert_eq!(runner.state().objects[&charm].zone, Zone::Hand);
    assert_eq!(hand_len(&runner, P0), hand, "Seven never drew");
    assert_eq!(dilemmas(&runner, P0), 0);

    // Positive: recast with the draw mode faces the dilemma.
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1);
    // The charm left the hand, drew two, and Seven drew exactly once (a trigger
    // leaked from the cancelled announcement would draw again).
    assert_eq!(hand_len(&runner, P0), hand - 1 + 2 + 1, "Seven drew once");
}

/// R10b — CR 602.2b + CR 733.1: an activated ability cancelled after its mode
/// choice faces no dilemma that is recorded, and its "whenever you face a
/// dilemma" trigger never reaches the stack.
#[test]
fn a_cancelled_activation_records_no_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let cankerbloom = scenario.add_real_card(P0, "Cankerbloom", Zone::Battlefield, db);
    // Two legal artifact targets (P0's Seven of Nine and P1's Sol Ring), so
    // the destroy-artifact mode raises its target prompt instead of
    // auto-targeting a single artifact.
    scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cankerbloom");
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choose the destroy-artifact mode");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "REACH: the destroy-artifact mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the activation");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the cancelled activation returns to the priority window: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the ability never reached the stack"
    );
    // The sacrifice is a commit-time cost, paid after the targets are chosen,
    // so nothing was sacrificed at the cancelled prompt.
    assert_eq!(
        runner.state().objects[&cankerbloom].zone,
        Zone::Battlefield,
        "the sacrifice cost was never paid"
    );
    assert_eq!(hand_len(&runner, P0), hand, "Seven never drew");
    assert_eq!(dilemmas(&runner, P0), 0);
    drive(&mut runner, Plan::default());

    // Positive: reactivate with the targetless proliferate mode (index 2);
    // that announcement reaches the stack, so the dilemma is recorded and
    // Seven drew once.
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reactivate Cankerbloom");
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![2]],
            ..Plan::default()
        },
    );
    assert_eq!(
        runner.state().objects[&cankerbloom].zone,
        Zone::Graveyard,
        "REACH: the sacrifice cost was paid this time"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven drew once");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R12 — CR 113.6m: Gabriel Lorca's ability works from the graveyard; paying
/// {B} returns it to its owner's hand.
#[test]
fn lorca_returns_from_the_graveyard_when_paid() {
    let Some(db) = shared_card_db() else { return };
    for accept in [true, false] {
        let mut scenario = board();
        let lorca = scenario.add_real_card(P0, "Gabriel Lorca, Mirror Menace", Zone::Graveyard, db);
        // Lorca's printed {B} in addition to Abzan Charm's {W}{B}{G}.
        scenario.add_basic_land(P0, ManaColor::Black);
        let charm = add_abzan_charm(&mut scenario, db);
        let mut runner = scenario.build();
        cast(&mut runner, charm);
        let record = drive(
            &mut runner,
            Plan {
                modes: vec![vec![1]],
                optional: vec![accept],
                ..Plan::default()
            },
        );
        assert_eq!(record.optional_actors, vec![P0], "accept {accept}");
        let zone = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Gabriel Lorca, Mirror Menace")
            .map(|object| object.zone);
        let expected = if accept { Zone::Hand } else { Zone::Graveyard };
        assert_eq!(zone, Some(expected), "accept {accept} ({lorca:?})");
    }
}

/// R13 — the per-turn history clears at the start of the next turn (the
/// card-level reading is Captain Benjamin Sisko's, phase 4).
#[test]
fn the_dilemma_history_resets_each_turn() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1, "REACH");
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    runner.pass_both_players();
    for _ in 0..16 {
        if runner.state().active_player == P1 {
            break;
        }
        runner.pass_both_players();
    }
    assert_eq!(runner.state().active_player, P1, "REACH: P1's turn began");
    assert_eq!(dilemmas(&runner, P0), 0);
}

/// R14 + R16 — Jett Reno and Melkotian Adjudicator each get one +1/+1 counter
/// for one dilemma.
#[test]
fn counter_creatures_grow_once_per_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let jett = scenario.add_real_card(P0, "Jett Reno, Problem Solver", Zone::Battlefield, db);
    let melkotian = scenario.add_real_card(P0, "Melkotian Adjudicator", Zone::Battlefield, db);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(plus_counters(&runner, jett), 1);
    assert_eq!(plus_counters(&runner, melkotian), 1);
}

/// R17 — CR 700.2b: Cult of Skaro's controller chooses its modes even when
/// the choice is "at random", so the nonempty random draw faces one dilemma:
/// Seven of Nine draws once and the history records one dilemma, with no mode
/// prompt.
#[test]
fn random_modes_face_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let cult = scenario.add_real_card(P0, "Cult of Skaro", Zone::Battlefield, db);
    let seven = add_seven(&mut scenario, P0, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let p1 = runner.life(P1);
    let permanents = runner.state().battlefield.len();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(cult, engine::game::combat::AttackTarget::Player(P1))])
        .expect("Cult of Skaro attacks");
    let record = drive(&mut runner, Plan::default());
    assert!(
        record.mode_choosers.is_empty(),
        "no mode prompt: {record:?}"
    );
    // Thay counters the artifact creature (Seven); Sec makes a Dalek; Jast
    // drains each opponent. Caan, the remaining mode, draws two.
    let thay = plus_counters(&runner, seven) > 0;
    let sec = runner.state().battlefield.len() > permanents;
    let jast = runner.life(P1) == p1 - 4;
    let other_modes = [thay, sec, jast].into_iter().filter(|&m| m).count();
    assert!(other_modes <= 1, "REACH: at most one random mode resolved");
    let caan_draw = if other_modes == 0 { 2 } else { 0 };
    assert_eq!(
        hand_len(&runner, P0),
        hand + caan_draw + 1,
        "the random mode resolved and Seven drew exactly one"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R21 — Tell Me What You See: "Whenever you face a dilemma, you may return
/// this enchantment to its owner's hand."
#[test]
fn tell_me_what_you_see_may_return_to_hand() {
    let Some(db) = shared_card_db() else { return };
    for accept in [true, false] {
        let mut scenario = board();
        let tell_me = scenario.add_real_card(P0, "Tell Me What You See", Zone::Battlefield, db);
        let charm = add_abzan_charm(&mut scenario, db);
        let mut runner = scenario.build();
        cast(&mut runner, charm);
        let record = drive(
            &mut runner,
            Plan {
                modes: vec![vec![1]],
                optional: vec![accept],
                ..Plan::default()
            },
        );
        assert_eq!(record.optional_actors, vec![P0]);
        let expected = if accept {
            Zone::Hand
        } else {
            Zone::Battlefield
        };
        let zone = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Tell Me What You See")
            .map(|object| object.zone);
        assert_eq!(zone, Some(expected), "accept {accept} ({tell_me:?})");
    }
}

/// R5b — "as you choose one or more modes": choosing zero modes for "Choose up
/// to two" faces no dilemma. Positive: the same turn's Abzan Charm does.
#[test]
fn zero_modes_face_no_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // Call Damage Control {1}{G}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let call = scenario.add_real_card(P0, "Call Damage Control", Zone::Hand, db);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, call);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![]],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the mode prompt was raised"
    );
    assert_eq!(hand_len(&runner, P0), hand - 1, "Seven did not draw");
    assert_eq!(dilemmas(&runner, P0), 0);

    cast(&mut runner, charm);
    drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R18 — a modal spell cast during another ability's resolution (Chaos Wand's
/// free cast) faces one dilemma, recorded once. Measured: this cast's events do
/// not pass through the resolution's action scan, so the scan exclusion
/// (`PlayerActionKind::turn_history_recorded_by_chain_scan`) is pinned by its
/// own unit test in `phase-ast`; this row pins the end-to-end count.
#[test]
fn a_free_cast_during_resolution_records_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let wand = scenario.add_real_card(P0, "Chaos Wand", Zone::Battlefield, db);
    // Chaos Wand's activation costs {4}.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let charm = scenario.add_real_card(P1, "Abzan Charm", Zone::Library, db);
    let mut runner = scenario.build();
    // `add_real_card` appends to the library (the bottom); Chaos Wand exiles
    // from the top, so re-seat the charm at index 0.
    let library = runner.state_mut().library_of_mut(P1);
    library.retain(|&object| object != charm);
    library.push_front(charm);
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: wand,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Chaos Wand");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the free cast chose modes"
    );
    assert_eq!(runner.life(P0), life - 2, "REACH: the charm resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand + 2 + 1,
        "the charm drew two, Seven one"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R1b — ruling "It doesn't matter if that spell or ability goes on to resolve
/// or not" (CR 700.17, edition effective 2026-11-06): a modal spell that is
/// countered after its modes were chosen still faced its dilemma. Abzan Charm's
/// draw mode is countered by Counterspell; Seven of Nine still draws once (its
/// trigger sits above the charm and resolves even though the charm does not),
/// the history holds one dilemma, and the charm's own draw and life loss never
/// happen. Fails if the dilemma were recorded only when the spell resolves.
#[test]
fn a_countered_modal_spell_still_faced_its_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let charm = add_abzan_charm(&mut scenario, db);
    // Counterspell {U}{U}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let counterspell = scenario.add_real_card(P0, "Counterspell", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, charm);
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choose the draw mode");
    cast(&mut runner, counterspell);
    drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Object(charm)],
            ..Plan::default()
        },
    );
    assert_eq!(
        runner.state().objects[&charm].zone,
        Zone::Graveyard,
        "REACH: the charm was countered"
    );
    assert_eq!(
        runner.life(P0),
        life,
        "REACH: the charm's life loss never happened"
    );
    // The charm and Counterspell left the hand; Seven drew once; the charm drew nothing.
    assert_eq!(hand_len(&runner, P0), hand - 2 + 1);
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R4b — CR 602.2b + CR 733.1 (positive path): an activated modal ability whose
/// chosen mode needs a target (Cankerbloom's "Destroy target artifact", two
/// legal artifacts so the target prompt is raised) faces one dilemma once it is
/// on the stack; Seven of Nine's trigger fires exactly once. The cancelled
/// counterpart is `a_cancelled_activation_records_no_dilemma`.
#[test]
fn a_targeted_modal_activation_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    let cankerbloom = scenario.add_real_card(P0, "Cankerbloom", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cankerbloom");
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![0]],
            targets: vec![TargetRef::Object(sol_ring)],
            ..Plan::default()
        },
    );
    assert_eq!(record.mode_choosers, vec![P0]);
    assert_eq!(
        runner.state().objects[&sol_ring].zone,
        Zone::Graveyard,
        "REACH: the targeted mode resolved"
    );
    assert_eq!(hand_len(&runner, P0), hand + 1, "Seven drew exactly one");
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// Casts Charming Prince from P0's hand and answers its enters trigger's mode
/// prompt with its third mode ("Exile another target creature you own"). P0 owns
/// two Bears and Seven of Nine besides the Prince, so the mode raises a target
/// prompt. Returns the runner at that `TriggerTargetSelection` prompt, the
/// Prince, Seven of Nine, the first Bear and P0's hand size before the cast.
fn prince_exile_mode_at_target_prompt(
    db: &CardDatabase,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, usize) {
    let mut scenario = board();
    let seven = add_seven(&mut scenario, P0, db);
    let bear = scenario.add_creature(P0, "Owned Bear A", 2, 2).id();
    scenario.add_creature(P0, "Owned Bear B", 2, 2);
    // Charming Prince {1}{W}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let prince = scenario.add_real_card(P0, "Charming Prince", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, prince);
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority so the Prince resolves");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { player: P0, .. }
        ),
        "REACH: the enters trigger asks P0 for its modes: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("choose the exile mode");
    eprintln!("PROBE0 after modes deferred={} stack={}", runner.state().deferred_triggers.len(), runner.state().stack.len());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "REACH: the exile mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    (runner, prince, seven, bear, hand)
}

/// R5c — CR 603.3c + CR 603.2 (construction span): while a modal trigger's
/// targeted mode still waits for its target, the entry under construction is
/// alone on the stack. The dilemma observer (Seven of Nine) has not been put
/// above that half-built entry and has not drawn; the precedent is the
/// target-lock events of `trigger_construction_target_events.rs`, which are held
/// until the construction ends. Charming Prince's exile mode with three legal
/// targets (two Bears and Seven of Nine).
#[test]
fn a_modal_trigger_waiting_for_its_target_has_no_observer_above_it() {
    let Some(db) = shared_card_db() else { return };
    let (runner, prince, _seven, _bear, hand) = prince_exile_mode_at_target_prompt(db);
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![prince],
        "only the Prince's trigger under construction is on the stack"
    );
    assert_eq!(hand_len(&runner, P0), hand - 1, "Seven has not drawn yet");
}

/// R5d — CR 603.3c + CR 700.17 + ruling "An ability that triggers whenever a
/// player faces a dilemma will resolve before the spell or ability that caused
/// it to trigger": once the target is chosen the stack is the Prince's trigger
/// with Seven of Nine's trigger above it, Seven resolves first (its draw
/// happens while the Bear is still on the battlefield), the dilemma is faced
/// exactly once, and the delayed-return trigger of the Prince's mode places no
/// second dilemma.
#[test]
fn a_targeted_modal_trigger_faces_one_dilemma_and_its_observer_resolves_first() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, prince, seven, bear, hand) = prince_exile_mode_at_target_prompt(db);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the first Bear");
    eprintln!("PROBE deferred={} pending_trigger={} dil={}", runner.state().deferred_triggers.len(), runner.state().pending_trigger.is_some(), dilemmas(&runner, P0));
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![prince, seven],
        "REACH: the Prince's trigger, with Seven's dilemma trigger above it"
    );
    runner.pass_both_players();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "REACH: exactly Seven's trigger resolved"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand,
        "Seven's draw resolved first (the Prince left the hand, Seven drew one)"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "the Prince's trigger has not resolved yet"
    );
    drive(&mut runner, Plan::default());
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "REACH: the exile mode resolved"
    );
    assert_eq!(hand_len(&runner, P0), hand, "Seven drew exactly once");
    assert_eq!(dilemmas(&runner, P0), 1);
}
