//! Group B of the dropped-qualifier census: cost-modification statics whose
//! subject names the "this turn" window and a spell class. The printed cards are
//! loaded from the committed integration fixture through `add_real_card` and
//! driven only through the public `GameRunner` actions.
//!
//! The parser change under test restores the named `spell_filter` on the granted
//! `ModifyCost` static ("instant and sorcery spells you cast this turn cost {1}
//! less", "artifact, instant, and sorcery spells ...", "artifact spells ...").
//! Every witness therefore pairs each reduced spell class with a control spell
//! class the printed card does NOT name (and, for "you cast" cards, with the
//! opponent's unaffected cost): the pre-fix parse reduced every spell, so the
//! controls fail on it.
//!
//! CR 601.2f (cost determination), CR 611.2a (a continuous effect from a
//! resolving spell or ability lasts as long as stated — here, this turn), and
//! CR 400.11 / CR 601.2a (casting from a zone) are the only rules this file
//! cites.
use engine::database::card_db::CardDatabase;
use engine::game::casting::display_spell_cost;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn generic_cost(generic: u32) -> ManaCost {
    cost(&[], generic)
}

// Printed costs of the spells used as reduced cases and controls.
fn printed_essence_scatter() -> ManaCost {
    cost(&[ManaCostShard::Blue], 1)
}

fn printed_essence_scatter_minus_one() -> ManaCost {
    cost(&[ManaCostShard::Blue], 0)
}

fn printed_essence_scatter_minus_two() -> ManaCost {
    cost(&[ManaCostShard::Blue], 0)
}

fn printed_divination_minus_two() -> ManaCost {
    cost(&[ManaCostShard::Blue], 0)
}

fn printed_divination() -> ManaCost {
    cost(&[ManaCostShard::Blue], 2)
}

fn printed_divination_minus_one() -> ManaCost {
    cost(&[ManaCostShard::Blue], 1)
}

fn printed_grizzly_bears() -> ManaCost {
    cost(&[ManaCostShard::Green], 1)
}

fn printed_sol_ring() -> ManaCost {
    generic_cost(1)
}

/// CR 601.2f: the {0} floor — a {2} reduction on a {1} artifact leaves a
/// zero-generic cost, never a negative one.
fn free_cost() -> ManaCost {
    generic_cost(0)
}

fn unit(kind: ManaType, index: u64) -> ManaUnit {
    ManaUnit::new(kind, ObjectId(7700 + index), false, vec![])
}

/// Replace `player`'s mana pool with exactly `kinds`, so a cast can only succeed
/// by paying exactly that amount and the pool reads empty afterwards.
fn fund(runner: &mut GameRunner, player: PlayerId, kinds: &[ManaType]) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    pool.clear();
    for (index, kind) in kinds.iter().enumerate() {
        pool.add(unit(*kind, index as u64));
    }
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn lore_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

fn loyalty_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

/// Read the cost the engine will charge `player` for `spell` right now.
fn assert_cost(
    runner: &GameRunner,
    player: PlayerId,
    spell: ObjectId,
    expected: ManaCost,
    what: &str,
) {
    assert_eq!(
        display_spell_cost(runner.state(), player, spell),
        Some(expected),
        "{what}"
    );
}

/// Resolve everything on the stack (draining ability-ordering prompts) and stop
/// at a closed `Priority` window with an empty stack.
fn settle_priority(runner: &mut GameRunner, what: &str) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority resolves the top of the stack");
            }
            other => panic!("{what}: unexpected prompt {other:?}"),
        }
    }
    panic!(
        "{what}: the stack did not settle, stuck at {:?}",
        runner.state().waiting_for
    );
}

/// Drive the combat steps with public actions (passing priority, declaring no
/// blockers) until `phase` is current.
fn advance_through_combat_to(runner: &mut GameRunner, phase: Phase, what: &str) {
    for _ in 0..40 {
        if runner.state().phase == phase {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("{what}: unexpected prompt {other:?} while advancing to {phase:?}"),
        }
    }
    panic!(
        "{what}: never reached {phase:?}, stuck at {:?} in {:?}",
        runner.state().waiting_for,
        runner.state().phase
    );
}

/// Advance the turn structure with public actions until P0's next precombat main
/// opens after turn `after_turn`. Stops early (returning the prompt) at any
/// prompt the caller must answer; stops when the main is reached with an empty
/// stack and no deferred ability waiting to be placed.
fn advance_to_next_p0_main(runner: &mut GameRunner, after_turn: u32, what: &str) -> WaitingFor {
    for _ in 0..800 {
        let state = runner.state();
        if state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && state.turn_number > after_turn
            && state.stack.is_empty()
            && state.deferred_triggers.is_empty()
            && matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0)
        {
            return state.waiting_for.clone();
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .unwrap_or_else(|error| panic!("{what}: pass priority rejected ({error:?})"));
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => return other,
        }
    }
    panic!(
        "{what}: never reached P0's next precombat main; stuck at {:?} in {:?}",
        runner.state().waiting_for,
        runner.state().phase
    );
}

// ===== Spellbinding Soprano =====

struct SopranoBoard {
    soprano: ObjectId,
    essence_scatter: ObjectId,
    essence_scatter_again: ObjectId,
    divination: ObjectId,
    divination_again: ObjectId,
    grizzly_bears: ObjectId,
    sol_ring: ObjectId,
    p1_essence_scatter: ObjectId,
}

fn soprano_board() -> (GameRunner, SopranoBoard) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 20]);
    scenario.with_library_top(P1, &["Island"; 20]);
    let soprano = scenario.add_real_card(P0, "Spellbinding Soprano", Zone::Battlefield, db());
    let board = SopranoBoard {
        soprano,
        essence_scatter: scenario.add_real_card(P0, "Essence Scatter", Zone::Hand, db()),
        essence_scatter_again: scenario.add_real_card(P0, "Essence Scatter", Zone::Hand, db()),
        divination: scenario.add_real_card(P0, "Divination", Zone::Hand, db()),
        divination_again: scenario.add_real_card(P0, "Divination", Zone::Hand, db()),
        grizzly_bears: scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db()),
        sol_ring: scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db()),
        p1_essence_scatter: scenario.add_real_card(P1, "Essence Scatter", Zone::Hand, db()),
    };
    (scenario.build(), board)
}

/// Declare Soprano as an attacker through the real action and resolve its
/// printed attack trigger (it targets nothing). The reduced costs asserted by the
/// callers are the behavioral proof that the trigger resolved.
fn soprano_attack_and_resolve_trigger(runner: &mut GameRunner, soprano: ObjectId) {
    runner.advance_to_combat();
    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "the declare-attackers step is current"
    );
    runner
        .declare_attackers(&[(soprano, AttackTarget::Player(P1))])
        .expect("declare Soprano as an attacker");
    settle_priority(runner, "Soprano's attack trigger");
}

/// Spellbinding Soprano's attack trigger: "Whenever this creature attacks,
/// instant and sorcery spells you cast this turn cost {1} less to cast." The
/// reduction covers exactly instant and sorcery spells (CR 601.2f), lasts for the
/// turn (CR 611.2a), and a spell cast from the hand is the recorded case
/// (CR 400.11 / CR 601.2a). Controls: a creature and an artifact keep their full
/// printed cost, and the opponent's instant is untouched because the card says
/// "you cast".
#[test]
fn spellbinding_soprano_attack_trigger_discounts_only_instant_and_sorcery_spells() {
    let (mut runner, board) = soprano_board();

    // The card's other printed clause: encore {3}{R}.
    assert!(
        runner.state().objects[&board.soprano]
            .has_keyword(&Keyword::Encore(cost(&[ManaCostShard::Red], 3))),
        "Spellbinding Soprano has encore {{3}}{{R}}"
    );

    // Before the attack trigger, every spell costs its printed price.
    assert_cost(
        &runner,
        P0,
        board.essence_scatter,
        printed_essence_scatter(),
        "an instant is unreduced before the attack trigger",
    );
    assert_cost(
        &runner,
        P0,
        board.divination,
        printed_divination(),
        "a sorcery is unreduced before the attack trigger",
    );
    assert_cost(
        &runner,
        P0,
        board.grizzly_bears,
        printed_grizzly_bears(),
        "the creature control is unreduced before the attack trigger",
    );
    assert_cost(
        &runner,
        P0,
        board.sol_ring,
        printed_sol_ring(),
        "the artifact control is unreduced before the attack trigger",
    );

    soprano_attack_and_resolve_trigger(&mut runner, board.soprano);

    // The named classes are reduced by {1}; the control classes are not.
    assert_cost(
        &runner,
        P0,
        board.essence_scatter,
        printed_essence_scatter_minus_one(),
        "the {1}{U} instant costs {U} in the combat window",
    );
    assert_cost(
        &runner,
        P0,
        board.divination,
        printed_divination_minus_one(),
        "the {2}{U} sorcery costs {1}{U} in the combat window",
    );
    assert_cost(
        &runner,
        P0,
        board.grizzly_bears,
        printed_grizzly_bears(),
        "creature spells are not named by the card and keep their full cost",
    );
    assert_cost(
        &runner,
        P0,
        board.sol_ring,
        printed_sol_ring(),
        "artifact spells are not named by the card and keep their full cost",
    );
    assert_cost(
        &runner,
        P1,
        board.p1_essence_scatter,
        printed_essence_scatter(),
        "P1's instant is unaffected: the card reduces spells YOU cast",
    );

    // Postcombat main: the window is still open (CR 611.2a, "this turn").
    advance_through_combat_to(&mut runner, Phase::PostCombatMain, "Soprano");
    settle_priority(&mut runner, "Soprano postcombat");

    // A full-price creature spell, then the reduced instant on the same stack.
    fund(&mut runner, P0, &[ManaType::Colorless, ManaType::Green]);
    let mut commit = runner.cast(board.grizzly_bears).commit();
    assert_eq!(
        commit.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the creature control pays its full {{1}}{{G}}"
    );
    assert_eq!(
        commit.state().objects[&board.grizzly_bears].zone,
        Zone::Stack,
        "the committed creature spell is on the stack"
    );
    {
        let pool = &mut commit.state_mut().players[P0.0 as usize].mana_pool;
        pool.clear();
        pool.add(unit(ManaType::Blue, 0));
    }
    let outcome = commit
        .cast(board.essence_scatter)
        .target_object(board.grizzly_bears)
        .resolve();
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "Essence Scatter pays exactly the reduced {{U}}"
    );
    assert_eq!(
        outcome.zone_of(board.essence_scatter),
        Zone::Graveyard,
        "the countered instant resolved and left the stack"
    );
    assert_eq!(
        outcome.zone_of(board.grizzly_bears),
        Zone::Graveyard,
        "the countered creature spell is put into its owner's graveyard"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the stack is empty after the counter spell resolves"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "P0 holds priority after the counter spell resolves, got {:?}",
        outcome.final_waiting_for()
    );
    settle_priority(&mut runner, "Soprano countered creature spell");

    // A sorcery cast in the postcombat main, still inside the window.
    fund(&mut runner, P0, &[ManaType::Colorless, ManaType::Blue]);
    let outcome = runner.cast(board.divination).resolve();
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "Divination pays exactly the reduced {{1}}{{U}} in the postcombat main"
    );
    assert_eq!(outcome.hand_drawn(P0), 2, "Divination draws two cards");
    assert_eq!(
        outcome.zone_of(board.divination),
        Zone::Graveyard,
        "the sorcery resolved"
    );
    settle_priority(&mut runner, "Soprano postcombat sorcery");
}

/// Spellbinding Soprano's window: the grant lasts only for the turn it was
/// created (CR 611.2a), so it is gone in P0's next precombat main. The same
/// printed instants and sorceries cost their printed price again (CR 601.2f),
/// cast from the hand as before (CR 400.11 / CR 601.2a).
#[test]
fn spellbinding_soprano_discount_lapses_at_end_of_turn() {
    let (mut runner, board) = soprano_board();
    soprano_attack_and_resolve_trigger(&mut runner, board.soprano);
    assert_cost(
        &runner,
        P0,
        board.essence_scatter,
        printed_essence_scatter_minus_one(),
        "the reduction is live in the attack's turn",
    );

    let turn = runner.state().turn_number;
    let waiting = advance_to_next_p0_main(&mut runner, turn, "Soprano's window lapsing");
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "P0's next precombat main opens with priority, got {waiting:?}"
    );
    assert!(
        runner.state().turn_number > turn,
        "the turn advanced past the attack turn"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);

    // The turn-scoped effect is gone; the printed prices are back.
    assert_cost(
        &runner,
        P0,
        board.essence_scatter_again,
        printed_essence_scatter(),
        "the instant costs {1}{U} again on the next turn",
    );
    assert_cost(
        &runner,
        P0,
        board.divination_again,
        printed_divination(),
        "the sorcery costs {2}{U} again on the next turn",
    );
    assert_cost(
        &runner,
        P0,
        board.sol_ring,
        printed_sol_ring(),
        "the artifact control is full price on the next turn",
    );
    settle_priority(&mut runner, "Soprano next turn");
}

// ===== Urza, Planeswalker =====

/// Urza, Planeswalker's `[+2]`: "Artifact, instant, and sorcery spells you cast
/// this turn cost {2} less to cast. You gain 2 life." The reduction covers the
/// three named classes (CR 601.2f) for the turn (CR 611.2a) and never goes below
/// {0}; a creature is the control the old filterless parse would have reduced.
/// The card's "You may activate the loyalty abilities of Urza twice each turn
/// rather than only once" is proven by a second successful activation in the same
/// turn. The spell is cast from the hand (CR 400.11 / CR 601.2a).
#[test]
fn urza_plus_two_discounts_artifacts_instants_and_sorceries_and_allows_a_second_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 12]);
    scenario.with_library_top(P1, &["Island"; 12]);
    let urza = scenario.add_real_card(P0, "Urza, Planeswalker", Zone::Battlefield, db());
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    let essence_scatter = scenario.add_real_card(P0, "Essence Scatter", Zone::Hand, db());
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    let grizzly_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let p1_sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Hand, db());
    let mut runner = scenario.build();

    // Printed loyalty 7, seeded as loyalty counters on entry.
    assert_eq!(loyalty_count(&runner, urza), 7);
    assert_eq!(runner.life(P0), 20);

    // Before the ability resolves, every spell costs its printed price.
    assert_cost(
        &runner,
        P0,
        sol_ring,
        printed_sol_ring(),
        "the artifact is unreduced before +2",
    );
    assert_cost(
        &runner,
        P0,
        essence_scatter,
        printed_essence_scatter(),
        "the instant is unreduced before +2",
    );
    assert_cost(
        &runner,
        P0,
        divination,
        printed_divination(),
        "the sorcery is unreduced before +2",
    );
    assert_cost(
        &runner,
        P0,
        grizzly_bears,
        printed_grizzly_bears(),
        "the creature control is unreduced before +2",
    );

    // Activate [+2] through the real loyalty-ability path.
    let outcome = runner.activate(urza, 0).resolve();
    assert_eq!(outcome.life_delta(P0), 2, "the +2 also gains 2 life");
    assert_eq!(loyalty_count(&runner, urza), 9, "+2 raises loyalty 7 -> 9");
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. })
            && outcome.state().stack.is_empty(),
        "the loyalty ability resolved and the stack is empty, got {:?}",
        outcome.final_waiting_for()
    );

    // Named classes: {2} less, floored at {0} for the artifact.
    assert_cost(
        &runner,
        P0,
        sol_ring,
        free_cost(),
        "Sol Ring's {1} becomes {0}, not a negative cost",
    );
    assert_cost(
        &runner,
        P0,
        essence_scatter,
        printed_essence_scatter_minus_two(),
        "the {1}{U} instant costs {U}",
    );
    assert_cost(
        &runner,
        P0,
        divination,
        printed_divination_minus_two(),
        "the {2}{U} sorcery costs {U}",
    );
    assert_cost(
        &runner,
        P0,
        grizzly_bears,
        printed_grizzly_bears(),
        "creature spells are not named and keep their full cost",
    );
    assert_cost(
        &runner,
        P1,
        p1_sol_ring,
        printed_sol_ring(),
        "P1's artifact is unaffected: the card reduces spells YOU cast",
    );

    // A reduced cast really succeeds with exactly the reduced mana: the free
    // artifact pays nothing from an empty pool.
    fund(&mut runner, P0, &[]);
    let outcome = runner.cast(sol_ring).resolve();
    assert_eq!(
        outcome.zone_of(sol_ring),
        Zone::Battlefield,
        "the reduced artifact resolves"
    );
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the artifact costs exactly {{0}}"
    );

    // A reduced sorcery pays exactly {U}.
    fund(&mut runner, P0, &[ManaType::Blue]);
    let outcome = runner.cast(divination).resolve();
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the reduced sorcery pays exactly {{U}}"
    );
    assert_eq!(outcome.hand_drawn(P0), 2);
    assert_eq!(outcome.zone_of(divination), Zone::Graveyard);

    // The control creature pays its full printed {1}{G}.
    fund(&mut runner, P0, &[ManaType::Colorless, ManaType::Green]);
    let outcome = runner.cast(grizzly_bears).resolve();
    assert_eq!(
        outcome.zone_of(grizzly_bears),
        Zone::Battlefield,
        "the full-price creature resolves"
    );
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the control creature pays its full {{1}}{{G}}"
    );

    // "twice each turn rather than only once": a second [+2] this turn is legal.
    let outcome = runner.activate(urza, 0).resolve();
    assert_eq!(
        loyalty_count(&runner, urza),
        11,
        "the second +2 is allowed this turn (7 -> 9 -> 11)"
    );
    assert_eq!(
        outcome.life_delta(P0),
        2,
        "the second activation gains 2 life"
    );
    settle_priority(&mut runner, "Urza's second activation");
}

// ===== Armor Wars =====

/// Armor Wars, chapter II: "Artifact spells you cast this turn cost {1} less to
/// cast." Chapter I's optional draw is answered at its prompt; the chapter II
/// grant installs on the lore counter added in P0's next precombat main
/// (CR 611.2a), reduces only artifact spells (CR 601.2f), and the artifact is
/// cast from the hand (CR 400.11 / CR 601.2a). A creature is the control class.
#[test]
fn armor_wars_chapter_two_discounts_only_artifact_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 16]);
    scenario.with_library_top(P1, &["Island"; 16]);
    let armor_wars = scenario.add_real_card(P0, "Armor Wars", Zone::Hand, db());
    // One artifact on the battlefield for chapter I's "for each artifact you
    // control", and one artifact spell in hand for chapter II.
    let battlefield_sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db());
    let hand_sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    let grizzly_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let p1_sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Hand, db());
    // {2}{U}{R}: exactly Armor Wars' printed cost.
    scenario.with_mana_pool(
        P0,
        vec![
            unit(ManaType::Colorless, 0),
            unit(ManaType::Colorless, 1),
            unit(ManaType::Blue, 2),
            unit(ManaType::Red, 3),
        ],
    );
    let mut runner = scenario.build();
    assert_eq!(
        zone_of(&runner, battlefield_sol_ring),
        Zone::Battlefield,
        "the artifact counted by chapter I is staged on the battlefield"
    );

    // The printed Saga is cast; payment happens at commit, entry and lore
    // counter at resolution.
    let commit = runner.cast(armor_wars).commit();
    assert_eq!(
        commit.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "Armor Wars pays exactly its printed {{2}}{{U}}{{R}}"
    );
    assert_eq!(
        commit.state().objects[&armor_wars].zone,
        Zone::Stack,
        "the committed Saga spell is on the stack"
    );

    // Chapter I: "You may draw a card for each artifact you control. If you do,
    // each opponent draws a card." Leave the optional choice open so it can be
    // asserted.
    let outcome = commit.stop_at_optional().resolve();
    assert_eq!(
        outcome.zone_of(armor_wars),
        Zone::Battlefield,
        "the Saga resolved onto the battlefield"
    );
    let lore = outcome.state().objects[&armor_wars]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0);
    assert_eq!(lore, 1, "the Saga enters with its first lore counter");

    // The chapter I pause belongs to the Saga's controller and names the Saga.
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "chapter I's optional draw must pause for its controller, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P0, "the Saga's controller answers chapter I");
    assert_eq!(*source_id, armor_wars, "the prompt names Armor Wars");

    // Accepting draws one card for the one artifact P0 controls, and each
    // opponent draws one card.
    let p0_before = runner.state().players[P0.0 as usize].hand.len();
    let p1_before = runner.state().players[P1.0 as usize].hand.len();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept chapter I's optional draw");
    settle_priority(&mut runner, "Armor Wars chapter I");
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_before + 1,
        "chapter I draws one card for the one artifact P0 controls"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_before + 1,
        "chapter I makes each opponent draw one card"
    );
    assert_eq!(lore_count(&runner, armor_wars), 1);

    // P0's next precombat main adds the second lore counter and triggers
    // chapter II.
    let turn = runner.state().turn_number;
    let waiting = advance_to_next_p0_main(&mut runner, turn, "Armor Wars chapter II");
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "P0's next precombat main opens with priority, got {waiting:?}"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        lore_count(&runner, armor_wars),
        2,
        "the next precombat main added the second lore counter"
    );
    assert_eq!(
        zone_of(&runner, armor_wars),
        Zone::Battlefield,
        "Armor Wars is past chapter I but not yet its final chapter"
    );

    // Chapter II: artifact spells cost {1} less; creature spells do not.
    assert_cost(
        &runner,
        P0,
        hand_sol_ring,
        free_cost(),
        "the {1} artifact spell costs {0}",
    );
    assert_cost(
        &runner,
        P0,
        grizzly_bears,
        printed_grizzly_bears(),
        "creature spells are not named by chapter II and keep their full cost",
    );
    assert_cost(
        &runner,
        P1,
        p1_sol_ring,
        printed_sol_ring(),
        "P1's artifact is unaffected: chapter II reduces spells YOU cast",
    );

    // The reduced artifact really casts with exactly the reduced mana.
    fund(&mut runner, P0, &[]);
    let outcome = runner.cast(hand_sol_ring).resolve();
    assert_eq!(
        outcome.zone_of(hand_sol_ring),
        Zone::Battlefield,
        "the reduced artifact resolves"
    );
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the artifact costs exactly {{0}}"
    );

    // The control creature pays its full printed {1}{G}.
    fund(&mut runner, P0, &[ManaType::Colorless, ManaType::Green]);
    let outcome = runner.cast(grizzly_bears).resolve();
    assert_eq!(
        outcome.zone_of(grizzly_bears),
        Zone::Battlefield,
        "the full-price creature resolves"
    );
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the control creature pays its full {{1}}{{G}}"
    );
    settle_priority(&mut runner, "Armor Wars chapter II");
}

// ===== Ballad of the Black Flag =====
//
// INFEASIBLE (no test written). Chapter IV installs "Historic spells you cast
// this turn cost {2} less to cast", but the resolving ability binds that grant to
// the Saga object itself: the parsed chapter's `GenericEffect` targets `SelfRef`
// and the install path turns that into `TargetFilter::SpecificObject` for the
// source (crates/engine/src/game/effects/effect.rs:637-662, the `SelfRef` arm of
// `bind_transient_effect`). A Saga is sacrificed by the state-based action as soon
// as its final chapter ability has left the stack
// (crates/engine/src/game/sba.rs:2426 `check_saga_sacrifice`), and that
// battlefield exit prunes every transient effect bound to the departed object
// (crates/engine/src/game/zones.rs:748 -> `prune_object_bound_effects_on_exit` at
// crates/engine/src/game/zones.rs:1330-1339, which calls
// crates/engine/src/game/layers.rs:1325-1333). The just-installed {2} reduction
// is therefore gone before the controller has priority to cast a historic spell.
// Witnessing the reduction would require casting before chapter IV resolves,
// which would weaken the control, so this card is left unwitnessed.

// ===== Izzet Steam Maze / The Pieces Are Coming Together =====
//
// INFEASIBLE (no test written). Both cards install their "this turn" grant bound
// to a card that never sits on the battlefield: Izzet Steam Maze is a Plane and
// The Pieces Are Coming Together is a Scheme, so both are object-bound grants to
// a command-zone object. Two engine facts block a runtime witness:
//
// 1. The `GrantStaticAbility` expansion only visits battlefield hosts
//    (crates/engine/src/game/layers.rs:8905-8920 iterates `state.battlefield`), so
//    a command-zone recipient never receives the granted `ModifyCost` static.
//    Even if it did, a non-emblem command-zone static functions only when its
//    `active_zones` opts in (crates/engine/src/game/functioning_abilities.rs:97-107),
//    and the parsed grant's `active_zones` is empty.
// 2. There is additionally no deterministic public driver for the chaos half:
//    `GameAction::RollPlanarDie` exists (crates/engine/src/game/engine.rs:12558)
//    but resolves a random planar-die face, so "chaos ensues" cannot be driven
//    deterministically. Setting a scheme in motion does have a public driver
//    (the archenemy precombat-main turn-based action), but it still installs its
//    grant object-bound to the scheme in the command zone, the same dead end as
//    step 1.
