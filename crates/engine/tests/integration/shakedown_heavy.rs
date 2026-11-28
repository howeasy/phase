//! Shakedown Heavy — CR 118.12: the defending player decides the draw; only
//! an accept untaps it and removes it from combat (CR 506.4). This module pins
//! the decline: Shakedown Heavy stays tapped and attacking. The accept branch
//! still binds "untap this creature" to the draw's parent target (a Shakedown
//! Heavy accept leaves it tapped), so the card keeps its confirmed-misparse row
//! and is not claimed as supported.
//!
//! "Whenever this creature attacks, defending player may have you draw a card.
//! If they do, untap this creature and remove it from combat."
//!
//! Shakedown Heavy is the printed card ({2}{B} Ogre Warrior), added from the
//! curated card database through `add_real_card`.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn attack(db: &CardDatabase, accept: bool) -> (GameRunner, ObjectId, Vec<PlayerId>, usize) {
    attack_players(db, accept, 2, P1)
}

/// P0's Shakedown Heavy attacks `defender` in a `players`-seat game; every seat's
/// prompt actor is recorded.
fn attack_players(
    db: &CardDatabase,
    accept: bool,
    players: u8,
    defender: PlayerId,
) -> (GameRunner, ObjectId, Vec<PlayerId>, usize) {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..players {
        for index in 0..3 {
            scenario.add_spell_to_library_top(PlayerId(seat), &format!("Library {index}"), false);
        }
    }
    let heavy = scenario.add_real_card(P0, "Shakedown Heavy", Zone::Battlefield, db);
    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(heavy, AttackTarget::Player(defender))])
        .expect("declare Shakedown Heavy as an attacker");
    let mut actors = Vec::new();
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                actors.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer Shakedown Heavy's draw choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return (runner, heavy, actors, hand_before);
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the attack trigger resolves");
            }
            other => panic!("unexpected Shakedown Heavy prompt: {other:?}"),
        }
    }
    panic!("Shakedown Heavy's attack trigger did not resolve");
}

fn is_attacking(runner: &GameRunner, heavy: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == heavy))
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

/// R4 — CR 118.12: on decline the controller does not draw and Shakedown Heavy
/// stays tapped and attacking; "untap … and remove it from combat" is the
/// "If they do" effect.
#[test]
fn decline_stays_tapped_and_attacking() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (runner, heavy, actors, hand_before) = attack(db, false);
    let tapped = runner.state().objects[&heavy].tapped;
    let attacking = is_attacking(&runner, heavy);
    assert_eq!(actors, vec![P1]);
    assert_eq!(hand_len(&runner), hand_before);
    assert!(tapped, "CR 118.12: a decline leaves Shakedown Heavy tapped");
    assert!(attacking, "CR 118.12: a decline leaves it attacking");
}

/// R4b — CR 506.2 + CR 608.2d: in a three-player game where P0 attacks P2, only
/// the defending player P2 is asked (neither P1, who is not attacked, nor the
/// controller); the decline leaves Shakedown Heavy tapped and attacking and the
/// controller draws nothing.
#[test]
fn three_players_decline_asks_only_the_defending_player() {
    const P2: PlayerId = PlayerId(2);
    let Some(db) = shared_card_db() else {
        return;
    };
    let (runner, heavy, actors, hand_before) = attack_players(db, false, 3, P2);
    assert_eq!(actors, vec![P2], "only the attacked player decides");
    assert_eq!(hand_len(&runner), hand_before);
    assert!(
        runner.state().objects[&heavy].tapped,
        "a decline leaves Shakedown Heavy tapped"
    );
    assert!(
        is_attacking(&runner, heavy),
        "a decline leaves it attacking"
    );
}

/// R4c — CR 506.2 + CR 118.12: the same three-player attack on P2, accepted. The
/// defending player P2 is the only one asked, and "you draw a card" is the
/// CONTROLLER's draw (CR 109.5), never the deciding defender's. The untap and
/// removal from combat are not asserted: the census row keeps the card
/// unsupported (see the module comment).
#[test]
fn three_players_accept_makes_the_controller_draw() {
    const P2: PlayerId = PlayerId(2);
    let Some(db) = shared_card_db() else {
        return;
    };
    let (runner, _heavy, actors, hand_before) = attack_players(db, true, 3, P2);
    assert_eq!(actors, vec![P2], "only the attacked player decides");
    assert_eq!(
        hand_len(&runner),
        hand_before + 1,
        "the controller drew the card the defender offered"
    );
    assert_eq!(
        runner.state().players[P2.0 as usize].hand.len(),
        0,
        "the deciding defender drew nothing"
    );
}
