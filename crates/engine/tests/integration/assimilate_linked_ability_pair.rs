//! Assimilation Assessor — a card exiled after its attack trigger latched by an
//! ability of ANOTHER copiable set of the same object is not "exiled with" the
//! trigger's ability (CR 607.5), though both belong to one object (CR 607.2a).
//!
//! Volrath, the Shapestealer ("{1}: Until your next turn, Volrath becomes a copy
//! of target creature with a counter on it, except it's 7/5 and it has this
//! ability.") copies Assimilation Assessor and attacks, putting the Assessor's
//! attack trigger on the stack. With that trigger still waiting, Volrath becomes
//! a copy of Intrepid Paleontologist and uses its "{2}: Exile target card from a
//! graveyard." on a creature card. The card is exiled by Volrath, but by an
//! ability that is not linked to the Assessor trigger, so the trigger's
//! "assimilate a creature card exiled with it" finds nothing and does nothing.
//!
//! Oracle (verbatim, MTGJSON 5.3.0+20261115):
//!   Volrath, the Shapestealer {2}{B}{G}{U}, Legendary Creature — Phyrexian
//!   Shapeshifter 7/5: "At the beginning of combat on your turn, put a -1/-1
//!   counter on up to one target creature.\n{1}: Until your next turn, Volrath
//!   becomes a copy of target creature with a counter on it, except it's 7/5 and
//!   it has this ability."
//!   Assimilation Assessor {4}{B}, Artifact Creature — Borg 5/5: "When this
//!   creature enters, exile target opponent's graveyard.\nWhenever this creature
//!   attacks, assimilate a creature card exiled with it. (Put it onto the
//!   battlefield under your control with a +1/+1 counter. It's a Borg artifact
//!   creature and loses all other creature types.)"
//!   Intrepid Paleontologist {1}{G}, Creature — Human Druid 2/2: "{T}: Add one
//!   mana of any color.\n{2}: Exile target card from a graveyard.\nYou may cast
//!   Dinosaur creature spells from among cards you own exiled with this
//!   creature. If you cast a spell this way, that creature enters with a
//!   finality counter on it. (If a creature with a finality counter on it would
//!   die, exile it instead.)"

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VOLRATH: &str = "At the beginning of combat on your turn, put a -1/-1 counter on up to one \
     target creature.\n\
     {1}: Until your next turn, Volrath becomes a copy of target creature with a counter on it, \
     except it's 7/5 and it has this ability.";

const ASSESSOR: &str = "When this creature enters, exile target opponent's graveyard.\n\
     Whenever this creature attacks, assimilate a creature card exiled with it. (Put it onto the \
     battlefield under your control with a +1/+1 counter. It's a Borg artifact creature and \
     loses all other creature types.)";

const PALEONTOLOGIST: &str = "{T}: Add one mana of any color.\n\
     {2}: Exile target card from a graveyard.\n\
     You may cast Dinosaur creature spells from among cards you own exiled with this creature. \
     If you cast a spell this way, that creature enters with a finality counter on it. (If a \
     creature with a finality counter on it would die, exile it instead.)";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// Activate `source`'s ability at `index`, answering a target prompt with
/// `target`, and leave it on the stack (it has not resolved).
fn activate_on_stack(runner: &mut GameRunner, source: ObjectId, index: usize, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: Default::default(),
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("target choice");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    panic!("activation did not reach priority");
}

/// Both players pass until the stack is one shorter.
fn resolve_one(runner: &mut GameRunner) {
    let before = runner.state().stack.len();
    for _ in 0..6 {
        if runner.state().stack.len() < before {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the top stack object did not resolve");
}

/// The index of the activated ability of `source` whose effect satisfies `pick`.
fn ability_index(runner: &GameRunner, source: ObjectId, pick: impl Fn(&Effect) -> bool) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|def| pick(&def.effect))
        .expect("the object has the activated ability")
}

#[test]
fn a_card_exiled_by_another_copiable_set_is_not_assimilated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Printed Volrath, the Shapestealer and the two creatures it copies; each
    // copied creature carries a counter, as Volrath's ability requires.
    let volrath = {
        let mut b =
            scenario.add_creature_from_oracle(P0, "Volrath, the Shapestealer", 7, 5, VOLRATH);
        b.as_legendary()
            .with_subtypes(vec!["Phyrexian", "Shapeshifter"])
            .with_mana_cost(cost(
                &[
                    ManaCostShard::Black,
                    ManaCostShard::Green,
                    ManaCostShard::Blue,
                ],
                2,
            ));
        b.id()
    };
    let assessor = {
        let mut b = scenario.add_creature_from_oracle(P0, "Assimilation Assessor", 5, 5, ASSESSOR);
        b.as_artifact_creature()
            .with_subtypes(vec!["Borg"])
            .with_mana_cost(cost(&[ManaCostShard::Black], 4))
            .with_plus_counters(1);
        b.id()
    };
    let paleontologist = {
        let mut b =
            scenario.add_creature_from_oracle(P0, "Intrepid Paleontologist", 2, 2, PALEONTOLOGIST);
        b.with_subtypes(vec!["Human", "Druid"])
            .with_mana_cost(cost(&[ManaCostShard::Green], 1))
            .with_plus_counters(1);
        b.id()
    };
    let victim = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Bear".to_string(),
        "Borg".to_string(),
        "Phyrexian".to_string(),
        "Shapeshifter".to_string(),
    ];
    runner
        .state_mut()
        .objects
        .get_mut(&volrath)
        .unwrap()
        .summoning_sick = false;

    // Volrath becomes a copy of the Assessor until P0's next turn.
    let copy_ability = ability_index(&runner, volrath, |e| matches!(e, Effect::BecomeCopy { .. }));
    activate_on_stack(&mut runner, volrath, copy_ability, assessor);
    resolve_one(&mut runner);
    assert_eq!(
        runner.state().objects[&volrath].name,
        "Assimilation Assessor"
    );

    // Attack: the copied attack trigger goes on the stack with an empty pile.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(volrath, AttackTarget::Player(P1))])
        .expect("declare the Assessor copy as attacker");
    assert_eq!(runner.state().stack.len(), 1, "the attack trigger waits");

    // Reach guard: nothing is exiled with Volrath yet.
    assert!(!runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.source_id == volrath));

    // With the trigger on the stack, Volrath becomes a copy of the Paleontologist
    // and exiles the creature card from P1's graveyard with ITS ability.
    let copy_ability = ability_index(&runner, volrath, |e| matches!(e, Effect::BecomeCopy { .. }));
    activate_on_stack(&mut runner, volrath, copy_ability, paleontologist);
    resolve_one(&mut runner);
    assert_eq!(
        runner.state().objects[&volrath].name,
        "Intrepid Paleontologist"
    );
    let exile_ability = ability_index(&runner, volrath, |e| {
        matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        )
    });
    activate_on_stack(&mut runner, volrath, exile_ability, victim);
    resolve_one(&mut runner);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == victim && link.source_id == volrath),
        "the exile is linked to Volrath by the Paleontologist ability: {:?}",
        runner.state().exile_links
    );

    // The Assessor trigger resolves: its pair exiled nothing, so nothing is
    // assimilated.
    resolve_one(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "settled: {:?} {:?}",
        runner.state().waiting_for,
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "a card exiled by another ability pair is not assimilated"
    );
}
