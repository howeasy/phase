//! Strefan, Maurer Progenitor — the end-step Blood-token count is "for each
//! player who lost life this turn", an all-players population.
//!
//! Oracle (verbatim, Scryfall):
//!   Flying
//!   At the beginning of your end step, create a Blood token for each player
//!   who lost life this turn.
//!   Whenever Strefan attacks, you may sacrifice two Blood tokens. If you do,
//!   you may put a Vampire card from your hand onto the battlefield tapped and
//!   attacking. It gains indestructible until end of turn.
//!
//! CR 608.2h + CR 119.3: the token count is read when the trigger resolves,
//! over every player (including the controller) whose life was lost this turn.
//! The parse must lower "for each player who lost life this turn" to a dynamic
//! `PlayerCount{PlayerAttribute{relation: All, attr: LifeLost, GE 1}}`
//! count, not a frozen `Fixed(1)`.
//!
//! Revert map: with the opponent-only for-each grammar reverted, the clause does
//! not parse, `try_parse_for_each_effect` declines, and the `Token` keeps
//! `count: Fixed(1)` — so this test observes 1 Blood token instead of 2. That is
//! the discriminating assertion.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const STREFAN: &str = "Flying\nAt the beginning of your end step, create a Blood token for each player who lost life this turn.\nWhenever Strefan attacks, you may sacrifice two Blood tokens. If you do, you may put a Vampire card from your hand onto the battlefield tapped and attacking. It gains indestructible until end of turn.";

const P2: PlayerId = PlayerId(2);

fn blood_tokens_on_battlefield(state: &engine::types::game_state::GameState) -> u32 {
    state
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name == "Blood"
        })
        .count() as u32
}

#[test]
fn strefan_end_step_creates_a_blood_token_per_player_who_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Strefan, Maurer Progenitor", 3, 2, STREFAN);
    let mut runner = scenario.build();

    // CR 119.3: seed this-turn life-loss history. The controller and P1 lost
    // life; P2 did not. "Each player" includes the controller, so the expected
    // count is 2 — an opponent-only read would answer 1.
    {
        let state = runner.state_mut();
        state.players[P0.0 as usize].life_lost_this_turn = 1;
        state.players[P1.0 as usize].life_lost_this_turn = 1;
        state.players[P2.0 as usize].life_lost_this_turn = 0;
    }

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        blood_tokens_on_battlefield(runner.state()),
        2,
        "one Blood token per player who lost life this turn (controller + P1), \
         not a frozen one; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// Printed attack trigger
// ---------------------------------------------------------------------------
//
// "Whenever Strefan attacks, you may sacrifice two Blood tokens. If you do, you
// may put a Vampire card from your hand onto the battlefield tapped and
// attacking." (Scryfall, verbatim; the same text lives in `STREFAN` above.)

/// Board for the attack-trigger tests: Strefan (on the battlefield, not
/// summoning-sick), a Vampire card in P0's hand, and `blood_count` Blood tokens
/// on P0's battlefield.
///
/// The Blood tokens are staged as artifact permanents with the `Blood` subtype
/// and marked as tokens after `build()`, mirroring the object the end-step
/// trigger creates (`Token { name: "Blood", types: ["Artifact", "Blood"] }`).
fn attack_board(blood_count: usize) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let strefan = scenario
        .add_creature_from_oracle(P0, "Strefan, Maurer Progenitor", 3, 2, STREFAN)
        .id();
    let vampire = scenario
        .add_creature_to_hand(P0, "Vampire Lacerator", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();
    let mut bloods = Vec::with_capacity(blood_count);
    for _ in 0..blood_count {
        bloods.push(
            scenario
                .add_creature(P0, "Blood", 0, 0)
                .as_artifact()
                .with_subtypes(vec!["Blood"])
                .id(),
        );
    }
    let mut runner = scenario.build();
    for &blood in &bloods {
        runner
            .state_mut()
            .objects
            .get_mut(&blood)
            .expect("the seeded Blood permanent exists")
            .is_token = true;
    }
    (runner, strefan, vampire, bloods)
}

/// CR 508.1: declare Strefan as the only attacker against P1.
fn attack_with_strefan(runner: &mut GameRunner, strefan: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(strefan, AttackTarget::Player(P1))])
        .expect("declaring Strefan as an attacker must succeed");
}

/// Drive Strefan's attack trigger to a settled priority window.
///
/// Returns the number of `OptionalEffectChoice` prompts answered. With
/// `accept == None` any such prompt is a test failure: the caller asserts the
/// offer is never made.
fn settle_attack_trigger(runner: &mut GameRunner, accept: Option<bool>) -> usize {
    let mut prompts = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                let decision = accept.unwrap_or_else(|| {
                    panic!(
                        "Strefan's attack trigger offered an optional effect the board \
                         cannot pay for: {:?}",
                        runner.state().waiting_for
                    )
                });
                prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: decision })
                    .expect("answering Strefan's optional effect must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompts,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority while resolving Strefan's trigger must succeed");
            }
            other => panic!(
                "unexpected waiting state while resolving Strefan's attack trigger: {other:?}"
            ),
        }
    }
    panic!("Strefan's attack trigger did not settle within 40 iterations");
}

/// Printed Oracle clause: "Whenever Strefan attacks, you may sacrifice two
/// Blood tokens. If you do, you may put a Vampire card from your hand onto the
/// battlefield tapped and attacking. It gains indestructible until end of
/// turn."
///
/// CR 608.2d: the player can't choose an option that's illegal or impossible.
/// CR 118.3: a player can't pay a cost without the resources to pay it fully.
/// With one Blood token, "sacrifice two Blood tokens" cannot be paid, so the
/// option must never be offered, the token must survive, and the gated Vampire
/// rider must not run.
#[test]
fn one_blood_token_does_not_offer_the_optional_sacrifice() {
    let (mut runner, strefan, vampire, bloods) = attack_board(1);
    attack_with_strefan(&mut runner, strefan);

    assert_eq!(
        settle_attack_trigger(&mut runner, None),
        0,
        "the infeasible sacrifice must auto-decline without ever surfacing \
         WaitingFor::OptionalEffectChoice; waiting_for = {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        blood_tokens_on_battlefield(runner.state()),
        1,
        "the lone Blood token must remain on the battlefield"
    );
    assert!(
        runner.state().battlefield.contains(&bloods[0]),
        "the lone Blood token must not be sacrificed"
    );
    assert_eq!(
        runner.state().objects[&vampire].zone,
        Zone::Hand,
        "the gated Vampire rider must not run when the sacrifice didn't happen"
    );
}

/// Printed Oracle clause: "Whenever Strefan attacks, you may sacrifice two
/// Blood tokens. If you do, you may put a Vampire card from your hand onto the
/// battlefield tapped and attacking. It gains indestructible until end of
/// turn."
///
/// CR 608.2d: the "you may sacrifice two" option is legal and payable with two
/// Blood tokens. CR 118.3: both tokens fully pay the printed cost. Accepting
/// sacrifices both, which unlocks the "If you do" rider offering the Vampire
/// card; the card is then put onto the battlefield tapped and attacking
/// (CR 508.4).
#[test]
fn two_blood_tokens_offer_the_sacrifice_then_put_a_vampire_onto_the_battlefield() {
    let (mut runner, strefan, vampire, bloods) = attack_board(2);
    attack_with_strefan(&mut runner, strefan);

    assert_eq!(
        settle_attack_trigger(&mut runner, Some(true)),
        2,
        "two prompts: the sacrifice offer, then the Vampire put-onto-the-battlefield offer"
    );

    assert_eq!(
        blood_tokens_on_battlefield(runner.state()),
        0,
        "accepting must sacrifice both Blood tokens"
    );
    for blood in &bloods {
        assert!(
            !runner.state().battlefield.contains(blood),
            "a sacrificed Blood token must leave the battlefield"
        );
    }

    let object = &runner.state().objects[&vampire];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "the Vampire card must move from hand to the battlefield"
    );
    assert_eq!(object.controller, P0);
    assert!(object.tapped, "printed clause: the Vampire enters tapped");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat is still active")
            .attackers
            .iter()
            .any(|attack| attack.object_id == vampire && attack.defending_player == P1),
        "printed clause + CR 508.4: the Vampire enters already attacking P1"
    );
}
