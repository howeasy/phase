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
//! no dilemma (a copy copies the chosen modes and chooses none, CR 700.2g). Ruling
//! 2025-06-06 (Umaro, Raging Yeti; Summon: Magus Sisters): "you choose a mode at
//! random", so a nonempty random draw faces one (CR 700.17).
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
    /// Answer a target prompt after the planned targets ran out by declining to
    /// choose another ("up to X target creatures", CR 601.2c).
    stop_choosing_targets: bool,
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
        stop_choosing_targets,
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
                let target = match targets.next() {
                    Some(target) => Some(target),
                    None if stop_choosing_targets => None,
                    None => panic!("unplanned target prompt"),
                };
                runner
                    .act(GameAction::ChooseTarget { target })
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

/// R12 — CR 113.6m: Gabriel Lorca, Mirror Menace ("Whenever you face a
/// dilemma, you may pay {B}. If you do, return this card from your graveyard to
/// your hand.") works from the graveyard; paying the {B} taps the black source.
/// The charm's {W}{B}{G} and Lorca's {B} are the only payments on the board, so
/// accepting taps both Swamps (one for each {B}) while declining leaves exactly
/// one tapped. The unpayable counterpart is `lorca_cannot_pay_without_black_mana`.
#[test]
fn lorca_returns_from_the_graveyard_when_paid() {
    let Some(db) = shared_card_db() else { return };
    for accept in [true, false] {
        let mut scenario = board();
        let lorca = scenario.add_real_card(P0, "Gabriel Lorca, Mirror Menace", Zone::Graveyard, db);
        // Lorca's printed {B} in addition to Abzan Charm's {W}{B}{G}. Both black
        // sources are held by id so each payment's tap is asserted exactly.
        let lorca_swamp = scenario.add_basic_land(P0, ManaColor::Black);
        let plains = scenario.add_basic_land(P0, ManaColor::White);
        let charm_swamp = scenario.add_basic_land(P0, ManaColor::Black);
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let charm = scenario.add_real_card(P0, "Abzan Charm", Zone::Hand, db);
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
        let expected = if accept { Zone::Hand } else { Zone::Graveyard };
        assert_eq!(
            runner.state().objects[&lorca].zone,
            expected,
            "accept {accept}"
        );
        // REACH: the charm's payment tapped its {W} and {G} lands either way.
        assert!(
            runner.state().objects[&plains].tapped,
            "the charm's {{W}} was paid (accept {accept})"
        );
        assert!(
            runner.state().objects[&forest].tapped,
            "the charm's {{G}} was paid (accept {accept})"
        );
        // The charm's {B} tapped exactly one Swamp; the other is the only black
        // source left, so accepting taps it for Lorca's {B} and declining
        // leaves it untapped.
        let (lorca_tapped, charm_tapped) = (
            runner.state().objects[&lorca_swamp].tapped,
            runner.state().objects[&charm_swamp].tapped,
        );
        if accept {
            assert!(
                lorca_tapped && charm_tapped,
                "both Swamps are tapped: the charm's {{B}} and Lorca's {{B}}"
            );
        } else {
            assert!(
                lorca_tapped != charm_tapped,
                "exactly one Swamp is tapped when the payment is declined"
            );
        }
    }
}

/// R12b — CR 608.2d: with no black source anywhere (a Charming Prince {1}{W}
/// board), Lorca's "you may pay {B}" is an impossible optional payment, which
/// is declined without opening a choice window: no optional prompt is raised,
/// the dilemma is still faced (the Prince's mode choice), and the card stays in
/// the graveyard. Fails if the unpayable offer opened an `OptionalEffectChoice`
/// prompt (the drive records every optional actor) or returned the card anyway.
#[test]
fn lorca_cannot_pay_without_black_mana() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let lorca = scenario.add_real_card(P0, "Gabriel Lorca, Mirror Menace", Zone::Graveyard, db);
    // Charming Prince {1}{W}: the dilemma source needs no black mana.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let prince = scenario.add_real_card(P0, "Charming Prince", Zone::Hand, db);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    cast(&mut runner, prince);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(runner.life(P0), life + 3, "REACH: the gain mode resolved");
    assert_eq!(dilemmas(&runner, P0), 1, "REACH: the dilemma was faced");
    assert!(
        record.optional_actors.is_empty(),
        "no optional prompt was raised: {record:?}"
    );
    assert_eq!(
        runner.state().objects[&lorca].zone,
        Zone::Graveyard,
        "the card stays in the graveyard"
    );
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

/// R17 — CR 700.17 (edition effective 2026-11-06): no CR rule says who chooses a
/// mode drawn at random; Cult of Skaro's draw is read as its controller choosing
/// its modes, so the nonempty random draw faces one dilemma:
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

/// R22 — CR 700.2b + CR 603.3c + CR 700.17 (edition effective 2026-11-06):
/// Panharmonicon ("If an artifact or creature entering causes a triggered
/// ability of a permanent you control to trigger, that ability triggers an
/// additional time.") doubles Charming Prince's enters trigger into two separate
/// triggered abilities, and the per-turn count "counts the number of spells
/// and/or abilities the player has chosen one or more modes for that turn", so
/// each instance's own mode choice faces its own dilemma: two prompts, two
/// records, two Seven of Nine draws from one entering event. Fails if the
/// doubled batch were counted as one dilemma, or if the second instance never
/// reached its mode choice.
#[test]
fn two_modal_triggers_in_one_batch_each_face_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    scenario.add_real_card(P0, "Panharmonicon", Zone::Battlefield, db);
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
            modes: vec![vec![1], vec![1]],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0, P0],
        "REACH: both doubled triggers asked for their modes"
    );
    assert_eq!(runner.life(P0), life + 6, "REACH: both gain modes resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 2,
        "Seven drew exactly twice"
    );
    assert!(
        runner.state().stack.is_empty(),
        "REACH: the batch resolved to an empty stack"
    );
    assert_eq!(dilemmas(&runner, P0), 2);
}

/// R22b — CR 700.2b + CR 603.3c + CR 700.2c: the same doubled batch with the
/// first Prince instance on the targeted mode ("Exile another target creature
/// you own", answered through the `TriggerTargetSelection` prompt) and the
/// second on "You gain 3 life". The first construction's span is released
/// after its own drain, so both Prince instances sit below both Seven triggers:
/// the first Seven draw resolves while the first Bear is still on the battlefield.
/// Fails if the deferred sibling were placed above the observers, or if either
/// construction recorded no dilemma.
#[test]
fn two_modal_triggers_first_targeted_each_face_a_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let seven = add_seven(&mut scenario, P0, db);
    let bear = scenario.add_creature(P0, "Owned Bear A", 2, 2).id();
    scenario.add_creature(P0, "Owned Bear B", 2, 2);
    scenario.add_real_card(P0, "Panharmonicon", Zone::Battlefield, db);
    // Charming Prince {1}{W}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let prince = scenario.add_real_card(P0, "Charming Prince", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, prince);
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { .. } => break,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority so the Prince resolves");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { player: P0, .. }
        ),
        "REACH: the first doubled trigger asks P0 for its modes: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("choose the exile mode on the first trigger");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "REACH: the exile mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the first Bear");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { player: P0, .. }
        ),
        "REACH: the second doubled trigger asks P0 for its modes: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choose the gain mode on the second trigger");
    assert_eq!(dilemmas(&runner, P0), 2);
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![prince, prince, seven, seven],
        "both Prince triggers below both Seven triggers"
    );
    runner.pass_both_players();
    assert_eq!(
        hand_len(&runner, P0),
        hand,
        "REACH: the top of the stack was Seven's draw"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "the first Bear survives the first Seven trigger"
    );
    drive(&mut runner, Plan::default());
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "REACH: the targeted mode resolved"
    );
    assert_eq!(runner.life(P0), life + 3, "REACH: the gain mode resolved");
    assert_eq!(
        hand_len(&runner, P0),
        hand + 1,
        "Seven drew exactly twice, the Prince left the hand"
    );
    assert_eq!(dilemmas(&runner, P0), 2);
    assert!(
        runner.state().stack.is_empty(),
        "REACH: the batch resolved to an empty stack"
    );
}

/// R1c — CR 601.2b + CR 700.2a + CR 700.2c: Abzan Charm ("Choose one —
/// • Exile target creature with power 3 or greater. • You draw two cards and
/// you lose 2 life. • Distribute two +1/+1 counters among one or two target
/// creatures.") cast to completion on its targeted mode faces exactly one
/// dilemma (CR 700.17, edition effective 2026-11-06): Seven of Nine draws once
/// while the charm exiles the chosen Giant and leaves the other alone. Fails if
/// a targeted modal spell recorded no dilemma, or one per prompt (mode plus
/// target).
#[test]
fn a_modal_spell_with_a_targeted_mode_faces_one_dilemma_when_cast_to_completion() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // Two legal exile targets, so the mode raises its target prompt.
    let giant = scenario.add_creature(P1, "P1 Giant", 4, 4).id();
    let second_giant = scenario.add_creature(P1, "P1 Second Giant", 4, 4).id();
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, charm);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![0]],
            targets: vec![TargetRef::Object(giant)],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the charm's mode prompt was answered"
    );
    assert_eq!(
        runner.state().objects[&giant].zone,
        Zone::Exile,
        "REACH: the targeted mode resolved"
    );
    assert_eq!(
        runner.state().objects[&second_giant].zone,
        Zone::Battlefield,
        "only the chosen target was exiled"
    );
    assert!(
        runner.state().stack.is_empty(),
        "REACH: the stack emptied at priority"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly once"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R5e — CR 603.3c + CR 700.2b + CR 700.2c: Elim Garak, Simple Tailor
/// ("Whenever you face a dilemma, each opponent loses 2 life and you gain 2
/// life.") is the only other creature P0 owns, so the Prince's "Exile another
/// target creature you own" mode has exactly one legal target and the engine
/// auto-selects it: no `TriggerTargetSelection` prompt, the construction still
/// faces its one dilemma, and per the ruling "An ability that triggers
/// whenever a player faces a dilemma will resolve before the spell or ability
/// that caused it to trigger" Garak's trigger resolves (P1 −2, P0 +2) while
/// Garak is still on the battlefield — the Prince's trigger exiles him
/// afterwards. Fails if the auto-targeted construction skipped the dilemma
/// record, or if the observer resolved only after the ability that caused it.
#[test]
fn a_modal_trigger_with_a_single_legal_target_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let garak = scenario.add_real_card(P0, "Elim Garak, Simple Tailor", Zone::Battlefield, db);
    // Charming Prince {1}{W}.
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let prince = scenario.add_real_card(P0, "Charming Prince", Zone::Hand, db);
    let mut runner = scenario.build();
    let lives = (runner.life(P0), runner.life(P1));
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
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the single legal target was auto-selected: {:?}",
        runner.state().waiting_for
    );
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![prince, garak],
        "REACH: the Prince's trigger with Garak's dilemma trigger above it"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
    runner.pass_both_players();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (lives.0 + 2, lives.1 - 2),
        "REACH: Garak's trigger resolved first, applied once"
    );
    assert_eq!(
        runner.state().objects[&garak].zone,
        Zone::Battlefield,
        "the Prince's trigger has not resolved yet"
    );
    drive(&mut runner, Plan::default());
    assert_eq!(
        runner.state().objects[&garak].zone,
        Zone::Exile,
        "REACH: the exile mode resolved"
    );
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (lives.0 + 2, lives.1 - 2),
        "Garak's effect applied exactly once"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R5f — CR 714.3a + CR 700.2b + CR 603.3c: Summon: Magus Sisters ("I, II,
/// III — Choose one at random — • Combine Powers! — Put three +1/+1 counters on
/// target creature. • Defense! — Put a shield counter on target creature. You
/// gain 3 life. • Fight! — This creature fights up to one target creature an
/// opponent controls.") enters with its first lore counter and chapter I draws
/// a mode through the seeded RNG. The P1 Bear is a legal target of every drawn
/// mode (the fight mode's "up to one" slot holds both the empty and the Bear
/// assignment, so no mode auto-selects), so whichever mode the fixed scenario
/// seed draws, the half-built chapter entry waits alone on the stack with no
/// Seven of Nine trigger above it and no dilemma recorded; the record and the
/// observer arrive only when the target choice finishes the construction,
/// exactly as on the prompted path. The outcome is asserted per drawn mode
/// (three +1/+1 counters, or a shield counter with 3 life, or the fight
/// killing the Bear) — the row pins the invariant, not one seed's draw. Fails
/// if the random draw recorded the dilemma before the construction finished,
/// or placed the observer above the half-built entry.
#[test]
fn a_random_modal_trigger_with_a_targeted_mode_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let seven = add_seven(&mut scenario, P0, db);
    let bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    // Summon: Magus Sisters {4}{G}.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let saga = scenario.add_real_card(P0, "Summon: Magus Sisters", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    let life = runner.life(P0);
    cast(&mut runner, saga);
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority so the Saga resolves");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "REACH: the drawn mode asks for its target: {:?}",
        runner.state().waiting_for
    );
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![saga],
        "only the half-built chapter trigger is on the stack"
    );
    assert_eq!(hand_len(&runner, P0), hand - 1, "Seven has not drawn yet");
    assert_eq!(
        dilemmas(&runner, P0),
        0,
        "the draw is latched, not recorded"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the Bear for the drawn mode");
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![saga, seven],
        "REACH: the chapter trigger, with Seven's dilemma trigger above it"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
    runner.pass_both_players();
    assert_eq!(
        hand_len(&runner, P0),
        hand,
        "REACH: Seven's draw resolved first"
    );
    drive(&mut runner, Plan::default());
    let shield = runner.state().objects[&bear]
        .counters
        .get(&CounterType::Shield)
        .copied()
        .unwrap_or(0);
    if runner.state().objects[&bear].zone == Zone::Graveyard {
        // Fight! was drawn: the 5/5 Saga fought the Bear.
        assert_eq!(
            (runner.life(P0), shield, plus_counters(&runner, bear)),
            (life, 0, 0),
            "the fight killed the Bear and nothing else applied"
        );
    } else if runner.life(P0) == life + 3 {
        // Defense! was drawn.
        assert_eq!(
            (shield, plus_counters(&runner, bear)),
            (1, 0),
            "the shield counter landed and nothing else applied"
        );
    } else {
        // Combine Powers! was drawn.
        assert_eq!(
            (plus_counters(&runner, bear), shield, runner.life(P0)),
            (3, 0, life),
            "three +1/+1 counters landed and nothing else applied"
        );
    }
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly once"
    );
    assert!(
        runner.state().stack.is_empty(),
        "REACH: the chapter resolved to an empty stack"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R23 — CR 700.17 + CR 603.12 + CR 700.2a: Slight Malfunction ("Choose one —
/// • Destroy target artifact. • Roll a six-sided die. When you do, Slight
/// Malfunction deals 1 damage to each of up to X target creatures, where X is
/// the result.") faces exactly one dilemma, when its mode is chosen at cast.
/// The reflexive "When you do" trigger is created during resolution from the
/// spell's context, which no longer carries the chooser latch, so it chooses no
/// modes and Seven of Nine does not draw a second time. Fails if the spell's
/// placement leaves the latch on the chain and the reflexive trigger faces a
/// second dilemma (Seven draws twice and the history holds two).
#[test]
fn a_modal_spell_with_a_reflexive_trigger_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let malfunction = scenario.add_real_card(P0, "Slight Malfunction", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, malfunction);
    let record = drive(
        &mut runner,
        Plan {
            modes: vec![vec![1]],
            targets: vec![TargetRef::Object(bear)],
            stop_choosing_targets: true,
            ..Plan::default()
        },
    );
    assert_eq!(
        record.mode_choosers,
        vec![P0],
        "REACH: the spell's mode prompt was answered once, by its controller"
    );
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        1,
        "REACH: the reflexive trigger resolved and dealt its 1 damage"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly once: the spell left the hand, one dilemma drew a card"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R24 — CR 700.17 + CR 601.2b + CR 107.3: Heliod's Intervention ("{X}{W}{W}
/// Instant. Choose one — • Destroy X target artifacts and/or enchantments. •
/// Target player gains twice X life.") announces X before its targets, so the
/// chosen mode's targeting is deferred past the X announcement. The spell still
/// faces exactly one dilemma, recorded once it is on the stack, and Seven of
/// Nine draws once. Fails if the X-deferred route never records the dilemma.
#[test]
fn an_x_deferred_modal_spell_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    add_seven(&mut scenario, P0, db);
    // {X}{W}{W} with X = 1.
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    let intervention = scenario.add_real_card(P0, "Heliod's Intervention", Zone::Hand, db);
    let mut runner = scenario.build();
    let hand = hand_len(&runner, P0);
    cast(&mut runner, intervention);
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choose the destroy mode");
    let mut chose_x = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 1 })
                    .expect("announce X = 1");
                chose_x = true;
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(sol_ring)),
                    })
                    .expect("choose the artifact");
            }
            _ => break,
        }
    }
    assert!(chose_x, "REACH: the spell asked for X");
    drive(&mut runner, Plan::default());
    assert_eq!(
        runner.state().objects[&sol_ring].zone,
        Zone::Graveyard,
        "REACH: the X-deferred mode resolved"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand - 1 + 1,
        "Seven drew exactly once"
    );
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R25 — CR 700.17 + CR 603.3c: Summon: Magus Sisters with no other creature
/// on the battlefield. Whichever mode the seeded draw picks has at most one
/// legal target (the Saga itself for the counter modes, none for "fights up to
/// one target creature an opponent controls"), so no mode prompts for a target
/// and the construction finishes inside the dispatch window. The chapter still
/// faces exactly one dilemma, read from the per-turn history (no observer is
/// needed, and every creature observer would be a second legal target). Fails
/// if the no-prompt random route records no dilemma, or two.
#[test]
fn a_random_modal_trigger_without_a_target_prompt_faces_one_dilemma() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    // Summon: Magus Sisters {4}{G}.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let saga = scenario.add_real_card(P0, "Summon: Magus Sisters", Zone::Hand, db);
    let mut runner = scenario.build();
    cast(&mut runner, saga);
    for _ in 0..8 {
        if runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == saga)
            && runner.state().stack.len() > 1
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority so the Saga resolves");
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "REACH: the drawn mode needed no target prompt: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        dilemmas(&runner, P0),
        1,
        "REACH: the chapter faced its dilemma when it was placed"
    );
    // Let the chapter resolve; the history still holds the one dilemma.
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority so the chapter resolves");
    }
    assert_eq!(dilemmas(&runner, P0), 1);
}

/// R26 — CR 603.3b + CR 700.2a: "An ability that triggers whenever a player
/// faces a dilemma will resolve before the spell or ability that caused it to
/// trigger." A modal spell (Abzan Charm) and a modal activation (Cankerbloom)
/// each leave Elim Garak, Simple Tailor's trigger above them on the stack.
/// Fails if the observer is placed below its cause.
#[test]
fn a_dilemma_observer_sits_above_the_spell_and_the_activation_that_caused_it() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = board();
    let garak = scenario.add_real_card(P0, "Elim Garak, Simple Tailor", Zone::Battlefield, db);
    let cankerbloom = scenario.add_real_card(P0, "Cankerbloom", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    let charm = add_abzan_charm(&mut scenario, db);
    let mut runner = scenario.build();
    let sources = |runner: &GameRunner| -> Vec<ObjectId> {
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect()
    };
    cast(&mut runner, charm);
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choose the draw mode");
    assert_eq!(
        sources(&runner),
        vec![charm, garak],
        "REACH: the spell with Garak's trigger above it"
    );
    drive(&mut runner, Plan::default());
    runner
        .act(GameAction::ActivateAbility {
            source_id: cankerbloom,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Cankerbloom");
    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("choose the destroy mode");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(sol_ring)),
            })
            .expect("choose the artifact");
    }
    assert_eq!(
        sources(&runner),
        vec![cankerbloom, garak],
        "REACH: the activation with Garak's trigger above it"
    );
    assert_eq!(dilemmas(&runner, P0), 2);
}
