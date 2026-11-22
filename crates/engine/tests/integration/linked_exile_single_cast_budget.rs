//! "Until end of turn, you may cast a [creature] spell from among cards exiled
//! with this artifact" authorizes ONE cast in all (CR 601.2a: "a spell"), across
//! every card of the linked pile, not one per card. The permissions one
//! resolution writes share a budget (`ExileWithAltCost::single_use_group`); the
//! first cast spends it and the rest of the pile is no longer castable that way.
//!
//! Oracle (verbatim, MTGJSON 5.3.0+20261115):
//!   Idol of Endurance {2}{W}, Artifact: "When this artifact enters, exile all
//!   creature cards with mana value 3 or less from your graveyard until this
//!   artifact leaves the battlefield.\n{1}{W}, {T}: Until end of turn, you may
//!   cast a creature spell from among cards exiled with this artifact without
//!   paying its mana cost."
//!   Serpent's Soul-Jar {2}{B}, Artifact: "Whenever an Elf you control dies,
//!   exile it.\n{T}, Pay 2 life: Until end of turn, you may cast a creature
//!   spell from among cards exiled with this artifact."

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::ExileLink;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IDOL_OF_ENDURANCE: &str = "When this artifact enters, exile all creature cards with mana \
     value 3 or less from your graveyard until this artifact leaves the battlefield.\n\
     {1}{W}, {T}: Until end of turn, you may cast a creature spell from among cards exiled with \
     this artifact without paying its mana cost.";

const SERPENTS_SOUL_JAR: &str = "Whenever an Elf you control dies, exile it.\n\
     {T}, Pay 2 life: Until end of turn, you may cast a creature spell from among cards exiled \
     with this artifact.";

const MURDER: &str = "Destroy target creature.";

const VOLTAIC_KEY: &str = "{1}, {T}: Untap target artifact.";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn cast_is_offered(runner: &GameRunner, card: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card),
    )
}

fn cast_to_resolution(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("the granted card is cast");
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the cast did not resolve");
}

fn tapped_count(runner: &GameRunner, lands: &[ObjectId]) -> usize {
    lands
        .iter()
        .filter(|land| runner.state().objects[land].tapped)
        .count()
}

/// Idol of Endurance exiles two creature cards from its controller's graveyard
/// when it enters; its activation grants one free creature cast over both.
#[test]
fn idol_of_endurance_grants_one_free_creature_cast_across_its_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = |scenario: &mut GameScenario, name: &str| {
        scenario
            .add_creature_to_graveyard(P0, name, 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(cost(&[ManaCostShard::Green], 1))
            .id()
    };
    let first = bear(&mut scenario, "Grizzly Bears");
    let second = bear(&mut scenario, "Runeclaw Bear");
    let idol = scenario
        .add_artifact_to_hand_from_oracle(P0, "Idol of Endurance", IDOL_OF_ENDURANCE)
        .with_mana_cost(cost(&[ManaCostShard::White], 2))
        .id();
    let lands: Vec<ObjectId> = (0..5)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let mut runner = scenario.build();

    // The Idol enters; its trigger exiles both cards, linked to it.
    runner.cast(idol).resolve();
    for card in [first, second] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert!(
            runner
                .state()
                .exile_links
                .iter()
                .any(|link: &ExileLink| link.exiled_id == card && link.source_id == idol),
            "{card:?} is exiled with the Idol"
        );
    }
    let spent = tapped_count(&runner, &lands);
    assert_eq!(spent, 3, "{{2}}{{W}} paid the Idol");

    // Before the activation neither card is castable.
    assert!(!cast_is_offered(&runner, first));

    runner.activate(idol, 0).resolve();
    assert_eq!(tapped_count(&runner, &lands), spent + 2, "{{1}}{{W}} paid");
    assert!(cast_is_offered(&runner, first));
    assert!(cast_is_offered(&runner, second));

    // One free cast, on either card; no mana is spent on it.
    cast_to_resolution(&mut runner, second);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 2,
        "the first cast is free"
    );
    assert!(
        !cast_is_offered(&runner, first),
        "the one cast the grant allows is spent: the other card is not castable"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ) && runner.state().stack.is_empty()
    );
}

/// Two activations of the Idol before any cast are two grants: two free casts
/// over the pile, no more (CR 611.2a). Voltaic Key's printed "{1}, {T}: Untap
/// target artifact." is what frees the Idol for the second activation.
#[test]
fn idol_of_endurance_activated_twice_grants_two_free_casts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = |scenario: &mut GameScenario, name: &str| {
        scenario
            .add_creature_to_graveyard(P0, name, 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(cost(&[ManaCostShard::Green], 1))
            .id()
    };
    let first = bear(&mut scenario, "Grizzly Bears");
    let second = bear(&mut scenario, "Runeclaw Bear");
    let idol = scenario
        .add_artifact_to_hand_from_oracle(P0, "Idol of Endurance", IDOL_OF_ENDURANCE)
        .with_mana_cost(cost(&[ManaCostShard::White], 2))
        .id();
    let key = scenario
        .add_artifact_from_oracle(P0, "Voltaic Key", VOLTAIC_KEY)
        .with_mana_cost(cost(&[], 1))
        .id();
    let lands: Vec<ObjectId> = (0..8)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let mut runner = scenario.build();

    runner.cast(idol).resolve();
    runner.activate(idol, 0).resolve();
    runner.activate(key, 0).target_object(idol).resolve();
    assert!(
        !runner.state().objects[&idol].tapped,
        "Voltaic Key untapped the Idol"
    );
    runner.activate(idol, 0).resolve();
    let spent = tapped_count(&runner, &lands);
    assert_eq!(
        spent, 8,
        "the Idol cast {{2}}{{W}}, its two activations {{1}}{{W}} each, and the Key's {{1}}"
    );
    assert!(cast_is_offered(&runner, first));
    assert!(cast_is_offered(&runner, second));

    cast_to_resolution(&mut runner, second);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(
        cast_is_offered(&runner, first),
        "the second activation's grant is still unspent"
    );
    cast_to_resolution(&mut runner, first);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &lands), spent, "both casts were free");
}

/// Serpent's Soul-Jar: the paid form. The Jar's printed dies trigger exiles two
/// Elves for real; one cast, paying its mana cost, spends the grant.
#[test]
fn serpents_soul_jar_grants_one_creature_cast_across_its_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario
        .add_artifact_from_oracle(P0, "Serpent's Soul-Jar", SERPENTS_SOUL_JAR)
        .with_mana_cost(cost(&[ManaCostShard::Black], 2))
        .id();
    let elf = |scenario: &mut GameScenario, name: &str| {
        scenario
            .add_creature(P0, name, 2, 3)
            .with_subtypes(vec!["Elf", "Warrior"])
            .with_mana_cost(cost(&[ManaCostShard::Green, ManaCostShard::Green], 0))
            .id()
    };
    let first = elf(&mut scenario, "Elvish Warrior");
    let second = elf(&mut scenario, "Elvish Warrior");
    let murders: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 1))
                .id()
        })
        .collect();
    let forests: Vec<ObjectId> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    // {1}{B}{B} twice: six black mana pays for both Murders in this main phase.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    // Real play: P0's own Elves die to printed Murder, and the Jar's trigger —
    // not a fixture — creates the linked exile the cast permission reads.
    for (murder, card) in murders.iter().zip([first, second]) {
        runner.cast(*murder).target_object(card).resolve();
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "the Jar's dies trigger exiled the dead Elf"
        );
        assert!(
            runner
                .state()
                .exile_links
                .iter()
                .any(|link: &ExileLink| link.exiled_id == card && link.source_id == jar),
            "{card:?} is exiled with the Jar"
        );
    }

    runner.activate(jar, 0).resolve();
    assert!(cast_is_offered(&runner, first));
    assert!(cast_is_offered(&runner, second));

    cast_to_resolution(&mut runner, first);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &forests),
        2,
        "the Jar's grant pays the printed cost {{G}}{{G}}"
    );
    assert!(
        !cast_is_offered(&runner, second),
        "the one cast the grant allows is spent: the other Elf is not castable"
    );
}
