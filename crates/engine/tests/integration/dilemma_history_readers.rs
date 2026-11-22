//! Readers of the per-turn dilemma history (CR 700.17, edition effective
//! 2026-11-06; the repo's pinned edition predates it: "faced a dilemma this
//! turn" means the player chose modes for a spell or ability this turn; the
//! mode choices are CR 700.2a / CR 700.2b).
//!
//! - Captain Benjamin Sisko: "At the beginning of your end step, create a 1/1
//!   red Officer creature token for each dilemma you've faced this turn."
//! - The Caretaker: "At the beginning of each end step, if you faced a dilemma
//!   this turn, target opponent exiles cards from the top of their library
//!   until they exile a nonland card. You may cast that card without paying its
//!   mana cost." The "if" is an intervening-if (CR 603.4); the free cast is
//!   CR 608.2g.
//!
//! Every named card below is the printed card, added from the curated card
//! database through `add_real_card`; Abzan Charm's printed {W}{B}{G} is paid
//! with basic lands.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

#[derive(Default)]
struct Plan {
    modes: Vec<Vec<usize>>,
    targets: Vec<TargetRef>,
    optional: Vec<bool>,
}

#[derive(Debug, Default)]
struct Record {
    target_prompts: usize,
    /// Target prompts raised while a triggered ability is put on the stack.
    trigger_target_prompts: usize,
    /// Legal targets of the first slot of each target prompt, in prompt order.
    slot_targets: Vec<Vec<TargetRef>>,
    optional_actors: Vec<PlayerId>,
}

/// Answer every prompt from `plan` until the stack is empty at priority.
fn drive(runner: &mut GameRunner, plan: Plan) -> Record {
    let mut modes = plan.modes.into_iter();
    let mut targets = plan.targets.into_iter();
    let mut optional = plan.optional.into_iter();
    let mut record = Record::default();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                let indices = modes.next().expect("unplanned mode prompt");
                runner
                    .act(GameAction::SelectModes { indices })
                    .expect("choose the planned modes");
            }
            WaitingFor::TargetSelection { target_slots, .. }
            | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                record.target_prompts += 1;
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::TriggerTargetSelection { .. }
                ) {
                    record.trigger_target_prompts += 1;
                }
                record.slot_targets.push(
                    target_slots
                        .first()
                        .map(|slot| slot.legal_targets.clone())
                        .unwrap_or_default(),
                );
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
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            // The turn walk in S3 passes through combat; nobody attacks or
            // blocks.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
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

/// Cast an Abzan Charm (its draw mode) from P0's hand: one faced dilemma.
fn face_one_dilemma(runner: &mut GameRunner, charm: ObjectId) {
    cast(runner, charm);
    drive(
        runner,
        Plan {
            modes: vec![vec![1]],
            ..Plan::default()
        },
    );
}

fn officers(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Officer")
        .count()
}

/// `add_real_card` stages a pre-existing permanent (not summoning-sick). Mark
/// `id` as having entered this turn (CR 302.6) so a turn walk's
/// declare-attackers step has no candidate and the engine auto-submits the
/// empty declaration (CR 508.1a): the walk reaches the end step without an
/// attack prompt.
fn mark_entered_this_turn(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.entered_battlefield_turn = Some(turn);
    object.summoning_sick = true;
}

fn sisko_board(charms: usize, db: &CardDatabase) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..8 {
            scenario.add_spell_to_library_top(
                player,
                &format!("Library {}-{index}", player.0),
                false,
            );
        }
    }
    let sisko = scenario.add_real_card(P0, "Captain Benjamin Sisko", Zone::Battlefield, db);
    let charms = (0..charms)
        .map(|_| {
            // Abzan Charm's printed cost is {W}{B}{G}: one land per shard.
            scenario.add_basic_land(P0, ManaColor::White);
            scenario.add_basic_land(P0, ManaColor::Black);
            scenario.add_basic_land(P0, ManaColor::Green);
            scenario.add_real_card(P0, "Abzan Charm", Zone::Hand, db)
        })
        .collect();
    let mut runner = scenario.build();
    mark_entered_this_turn(&mut runner, sisko);
    (runner, charms)
}

/// S1 — one Officer per dilemma faced this turn: two dilemmas, two Officers.
#[test]
fn sisko_creates_an_officer_per_dilemma() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, charms) = sisko_board(2, db);
    for charm in charms {
        face_one_dilemma(&mut runner, charm);
    }
    assert_eq!(officers(&runner), 0, "REACH: none before the end step");
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 2);
}

/// S2 — no dilemma this turn, no Officer.
#[test]
fn sisko_creates_nothing_without_a_dilemma() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, _) = sisko_board(0, db);
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 0);
}

/// S3 — "this turn": a dilemma faced on an earlier turn of P0's is not counted
/// at a later end step of P0's.
#[test]
fn sisko_counts_only_this_turns_dilemmas() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, charms) = sisko_board(1, db);
    face_one_dilemma(&mut runner, charms[0]);
    runner.advance_to_end_step();
    drive(&mut runner, Plan::default());
    assert_eq!(
        officers(&runner),
        1,
        "REACH: the first end step counted one"
    );
    // Through P1's turn to P0's next end step.
    for _ in 0..64 {
        runner.pass_both_players();
        drive(&mut runner, Plan::default());
        if runner.state().active_player == P0 && runner.state().phase == Phase::End {
            break;
        }
    }
    assert_eq!(runner.state().active_player, P0, "REACH: P0's next turn");
    assert_eq!(runner.state().phase, Phase::End, "REACH: its end step");
    drive(&mut runner, Plan::default());
    assert_eq!(officers(&runner), 1, "no Officer for last turn's dilemma");
}

fn caretaker_board(db: &CardDatabase) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..5 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Library {index}"), false);
    }
    // P1's library top reads: land, land, Sacred Nectar (a sorcery).
    scenario.add_real_card(P1, "Sacred Nectar", Zone::Library, db);
    scenario.add_land_to_library_top(P1, "P1 Land A");
    scenario.add_land_to_library_top(P1, "P1 Land B");
    let caretaker = scenario.add_real_card(P0, "The Caretaker", Zone::Battlefield, db);
    // Abzan Charm's printed cost is {W}{B}{G}: one land per shard.
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Green);
    let charm = scenario.add_real_card(P0, "Abzan Charm", Zone::Hand, db);
    let mut runner = scenario.build();
    mark_entered_this_turn(&mut runner, caretaker);
    (runner, charm)
}

fn exiled(runner: &GameRunner, name: &str) -> bool {
    runner
        .state()
        .objects
        .values()
        .any(|object| object.name == name && object.zone == Zone::Exile)
}

/// C1 — CR 603.4 + CR 608.2g: after a dilemma, at the end step P1 exiles
/// until a nonland card, and P0 may cast that card free (Sacred Nectar: P0
/// gains 4).
#[test]
fn caretaker_exiles_until_nonland_and_offers_the_free_cast() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, charm) = caretaker_board(db);
    face_one_dilemma(&mut runner, charm);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Player(P1)],
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(record.optional_actors, vec![P0]);
    assert!(exiled(&runner, "P1 Land A") && exiled(&runner, "P1 Land B"));
    assert_eq!(runner.life(P0), life + 4, "the free cast resolved");
}

/// Three-player witness: P0 controls The Caretaker and holds an Abzan Charm;
/// P2's library top reads land, land, Sacred Nectar (the nonland stop), then a
/// Mountain sentinel; P1's library is a distinct stack no effect may touch.
struct CaretakerThreePlayer {
    runner: GameRunner,
    charm: ObjectId,
    /// P2's top card (a land).
    p2_top_land: ObjectId,
    /// P2's second card (a land).
    p2_land: ObjectId,
    /// P2's third card, the nonland that stops the exile loop.
    p2_nectar: ObjectId,
    /// P2's fourth card, under the nonland: it must stay in the library.
    p2_sentinel: ObjectId,
}

fn caretaker_three_player(db: &CardDatabase) -> CaretakerThreePlayer {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..5 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Library {index}"), false);
    }
    // Appended in this order, then topped by the two lands: the library reads
    // land, land, Sacred Nectar, Mountain from the top.
    let p2_nectar = scenario.add_real_card(P2, "Sacred Nectar", Zone::Library, db);
    let p2_sentinel = scenario.add_real_card(P2, "Mountain", Zone::Library, db);
    let p2_land = scenario.add_land_to_library_top(P2, "P2 Land A").id();
    let p2_top_land = scenario.add_land_to_library_top(P2, "P2 Land B").id();
    for index in 0..3 {
        scenario.add_spell_to_library_top(P1, &format!("P1 Library {index}"), false);
    }
    let caretaker = scenario.add_real_card(P0, "The Caretaker", Zone::Battlefield, db);
    // Abzan Charm's printed cost is {W}{B}{G}: one land per shard.
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Green);
    let charm = scenario.add_real_card(P0, "Abzan Charm", Zone::Hand, db);
    let mut runner = scenario.build();
    mark_entered_this_turn(&mut runner, caretaker);
    CaretakerThreePlayer {
        runner,
        charm,
        p2_top_land,
        p2_land,
        p2_nectar,
        p2_sentinel,
    }
}

fn library_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// CR 700.17 (edition effective 2026-11-06) + CR 603.4 + CR 608.2g: The
/// Caretaker — "At the beginning of each end step, if you faced a dilemma this
/// turn, target opponent exiles cards from the top of their library until they
/// exile a nonland card. You may cast that card without paying its mana cost."
/// In a three-player game the trigger declares one player target (CR 115.1d)
/// and BOTH opponents are legal (CR 115.1); only the chosen opponent's library
/// is touched, and P0 casts the exiled nonland card free.
///
/// Mutants killed: resolving "target opponent" as the first opponent (or the
/// controller) without asking, and exiling from every opponent's library.
#[test]
fn the_caretaker_targets_the_chosen_opponent_in_a_three_player_game() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let CaretakerThreePlayer {
        mut runner,
        charm,
        p2_top_land,
        p2_land,
        p2_nectar,
        p2_sentinel,
    } = caretaker_three_player(db);
    let p2_library = library_ids(&runner, P2);
    assert_eq!(
        p2_library,
        vec![p2_top_land, p2_land, p2_nectar, p2_sentinel],
        "REACH: P2's stack, top first"
    );
    let p1_library = library_ids(&runner, P1);
    face_one_dilemma(&mut runner, charm);
    assert_eq!(
        runner.state().objects[&charm].zone,
        Zone::Graveyard,
        "REACH: the modal spell resolved and recorded the dilemma"
    );
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Player(P2)],
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.trigger_target_prompts, 1,
        "REACH: the Caretaker's trigger asks for a target: {record:?}"
    );
    assert_eq!(
        record.slot_targets.len(),
        1,
        "the trigger declares a player target: {record:?}"
    );
    let legal = &record.slot_targets[0];
    assert_eq!(legal.len(), 2, "both opponents are legal: {legal:?}");
    assert!(
        legal.contains(&TargetRef::Player(P1)) && legal.contains(&TargetRef::Player(P2)),
        "both opponents are legal: {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Player(P0)),
        "the controller is not an opponent: {legal:?}"
    );
    assert_eq!(
        record.optional_actors,
        vec![P0],
        "REACH: the controller was offered the free cast"
    );
    // The loop exiled P2's two lands and the nonland, then stopped; the free
    // cast moved that nonland out of exile and resolved it.
    let exile: Vec<ObjectId> = runner.state().exile.iter().copied().collect();
    assert_eq!(
        exile,
        vec![p2_top_land, p2_land],
        "the exact cards still exiled after the free cast"
    );
    assert_eq!(
        runner.state().objects[&p2_nectar].zone,
        Zone::Graveyard,
        "the exiled nonland spell was cast and resolved"
    );
    assert_eq!(
        library_ids(&runner, P2),
        vec![p2_sentinel],
        "the loop stopped at the nonland card"
    );
    assert_eq!(
        library_ids(&runner, P1),
        p1_library,
        "P1's library is untouched (same ids and length)"
    );
    assert_eq!(runner.life(P0), life + 4, "Sacred Nectar's printed effect");
}

/// CR 700.17 (edition effective 2026-11-06) + CR 603.4 + CR 608.2g: The
/// Caretaker's free cast — "You may cast that card without paying its mana
/// cost." — is optional. Declining leaves the exiled nonland card in exile and
/// casts nothing.
///
/// Mutant killed: a mandatory free cast that ignores the declined optional.
#[test]
fn the_caretaker_free_cast_can_be_declined() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let CaretakerThreePlayer {
        mut runner,
        charm,
        p2_top_land,
        p2_land,
        p2_nectar,
        p2_sentinel,
    } = caretaker_three_player(db);
    let p1_library = library_ids(&runner, P1);
    face_one_dilemma(&mut runner, charm);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(
        &mut runner,
        Plan {
            targets: vec![TargetRef::Player(P2)],
            optional: vec![false],
            ..Plan::default()
        },
    );
    assert_eq!(
        record.optional_actors,
        vec![P0],
        "REACH: the controller was offered the free cast"
    );
    let exile: Vec<ObjectId> = runner.state().exile.iter().copied().collect();
    assert_eq!(
        exile,
        vec![p2_top_land, p2_land, p2_nectar],
        "the exiled nonland card stays in exile"
    );
    assert_eq!(
        runner.state().objects[&p2_nectar].zone,
        Zone::Exile,
        "nothing was cast"
    );
    assert!(runner.state().stack.is_empty(), "the stack is empty");
    assert_eq!(
        library_ids(&runner, P2),
        vec![p2_sentinel],
        "the loop still stopped at the nonland card"
    );
    assert_eq!(
        library_ids(&runner, P1),
        p1_library,
        "P1's library is untouched"
    );
    assert_eq!(runner.life(P0), life, "nothing resolved for P0");
}

/// C2 — CR 603.4: with no dilemma this turn the ability does not trigger.
#[test]
fn caretaker_does_not_trigger_without_a_dilemma() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, _) = caretaker_board(db);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Plan::default());
    assert_eq!(record.target_prompts, 0);
    assert!(record.optional_actors.is_empty());
    assert!(!exiled(&runner, "P1 Land A"));
    assert_eq!(runner.life(P0), life);
}

/// C2b — ruling "The Caretaker's last ability will check as your end step
/// starts ... Casting a modal spell or activating a modal ability during your
/// end step won't cause the ability to trigger": a dilemma faced after the end
/// step began does not trigger it. Positive reach: the same Abzan Charm is cast
/// and resolves in that end step, and C1 above shows the trigger firing when
/// the dilemma came first.
#[test]
fn caretaker_ignores_a_dilemma_faced_during_the_end_step() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, charm) = caretaker_board(db);
    runner.advance_to_end_step();
    let record = drive(&mut runner, Plan::default());
    assert_eq!(
        record.target_prompts, 0,
        "REACH: no dilemma yet, so the end-step trigger did not fire"
    );
    assert_eq!(runner.state().phase, Phase::End, "REACH: the end step");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "REACH: P0 holds priority in the end step: {:?}",
        runner.state().waiting_for
    );
    face_one_dilemma(&mut runner, charm);
    assert_eq!(
        runner.state().objects[&charm].zone,
        Zone::Graveyard,
        "REACH: the modal spell was cast and resolved in the end step"
    );
    assert!(
        !exiled(&runner, "P1 Land A"),
        "the Caretaker does not trigger for a dilemma faced after its check"
    );
}

/// S4 — Sisko's activated ability: exile the top five; P0 may cast spells
/// among them with mana value ≤ the number of Officers P0 controls (three
/// here: Sisko is a Human Officer, plus two Officer tokens) without paying
/// their mana costs (CR 608.2g). Sacred Nectar (mana value 2, CR 202.3) is
/// castable, Divination (mana value 3, exactly the count) is castable, and
/// Angel's Mercy (mana value 4, one over the count) is not.
///
/// Mutant killed: reading the printed "less than or equal to" as "<" drops
/// Divination (the equality case).
#[test]
fn sisko_casts_only_spells_within_the_officer_count() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Top five, in order: two fillers, Angel's Mercy, Divination, Sacred
    // Nectar. The appended lands sit under them so Divination's two draws do
    // not deck P0.
    let angel = scenario.add_real_card(P0, "Angel's Mercy", Zone::Library, db);
    let divination = scenario.add_real_card(P0, "Divination", Zone::Library, db);
    let nectar = scenario.add_real_card(P0, "Sacred Nectar", Zone::Library, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }
    for index in 0..2 {
        scenario.add_spell_to_library_top(P0, &format!("P0 Filler {index}"), false);
    }
    let sisko = scenario.add_real_card(P0, "Captain Benjamin Sisko", Zone::Battlefield, db);
    for _ in 0..2 {
        scenario
            .add_creature(P0, "Officer", 1, 1)
            .with_subtypes(vec!["Officer"]);
    }
    let mana = [
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ]
    .into_iter()
    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
    .collect();
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: sisko,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Sisko");
    let record = drive_casting_from_among(&mut runner, &["Divination", "Sacred Nectar"]);
    assert!(
        record.offered.iter().any(|name| name == "Sacred Nectar"),
        "REACH: the mana value 2 spell is offered: {record:?}"
    );
    assert!(
        record.offered.iter().any(|name| name == "Divination"),
        "the mana value 3 spell equals the three Officers and is offered: {record:?}"
    );
    assert!(
        !record.offered.iter().any(|name| name == "Angel's Mercy"),
        "the mana value 4 spell exceeds the Officer count: {record:?}"
    );
    assert_eq!(runner.life(P0), life + 4, "Sacred Nectar resolved");
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        2,
        "Divination drew two cards"
    );
    assert_eq!(
        runner.state().objects[&divination].zone,
        Zone::Graveyard,
        "the equality-case spell was cast"
    );
    assert_eq!(
        runner.state().objects[&nectar].zone,
        Zone::Graveyard,
        "Sacred Nectar was cast"
    );
    assert_eq!(
        runner.state().objects[&angel].zone,
        Zone::Exile,
        "the one-over spell was not cast"
    );
}

/// The names each free-cast window offered, in order.
#[derive(Debug, Default)]
struct FromAmong {
    offered: Vec<String>,
}

/// Resolve Sisko's ability: at each free-cast window record every offered
/// card, cast each name in `to_cast` once if offered, and otherwise stop
/// casting.
fn drive_casting_from_among(runner: &mut GameRunner, to_cast: &[&str]) -> FromAmong {
    let mut record = FromAmong::default();
    let mut cast: Vec<String> = Vec::new();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
                ..
            } => {
                let names: Vec<String> = candidates
                    .iter()
                    .map(|id| runner.state().objects[id].name.clone())
                    .collect();
                record.offered.extend(names.iter().cloned());
                let selection = candidates
                    .iter()
                    .zip(&names)
                    .find(|(_, name)| {
                        let name: &str = name.as_str();
                        to_cast.contains(&name) && !cast.iter().any(|done| done.as_str() == name)
                    })
                    .map(|(id, _)| *id);
                if let Some(id) = selection {
                    cast.push(runner.state().objects[&id].name.clone());
                }
                runner
                    .act(GameAction::FreeCastWindowChoice { selection })
                    .expect("answer the free-cast window");
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
    panic!("Sisko's ability did not resolve: {record:?}");
}

/// C3 — "each end step": a dilemma P0 faces on P1's turn (an instant-speed
/// modal spell) makes The Caretaker trigger at P1's end step.
#[test]
fn caretaker_triggers_at_an_opponents_end_step() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut runner, charm) = caretaker_board(db);
    for _ in 0..64 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner.pass_both_players();
        drive(&mut runner, Plan::default());
    }
    assert_eq!(runner.state().active_player, P1, "REACH: P1's turn");
    // P1 passes priority in its main phase, so P0 may cast the instant.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "REACH: P0 holds priority on P1's turn: {:?}",
        runner.state().waiting_for
    );
    face_one_dilemma(&mut runner, charm);
    let life = runner.life(P0);
    runner.advance_to_end_step();
    assert_eq!(runner.state().active_player, P1, "REACH: P1's end step");
    let record = drive(
        &mut runner,
        Plan {
            optional: vec![true],
            ..Plan::default()
        },
    );
    assert_eq!(record.optional_actors, vec![P0]);
    assert_eq!(runner.life(P0), life + 4, "the free cast resolved");
}
