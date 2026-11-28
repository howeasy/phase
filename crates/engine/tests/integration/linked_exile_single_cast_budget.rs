//! "Until end of turn, you may cast a [creature] spell from among cards exiled
//! with this artifact" authorizes ONE cast in all (CR 608.2c: "a spell" is one cast), across
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
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastPaymentMode, ExileLink, WaitingFor};
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

const ETERNAL_SCOURGE: &str = "You may cast this card from exile.\n\
     When this creature becomes the target of a spell or ability an opponent controls, exile this \
     creature.";

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

/// A cast Idol of Endurance with Eternal Scourge and Grizzly Bears exiled by
/// its entry trigger. `land_count` white lands are untapped at the start.
struct IdolScourgePile {
    runner: GameRunner,
    idol: ObjectId,
    scourge: ObjectId,
    bears: ObjectId,
    lands: Vec<ObjectId>,
}

fn idol_scourge_pile(land_count: usize) -> IdolScourgePile {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scourge = scenario
        .add_creature_to_graveyard(P0, "Eternal Scourge", 3, 3)
        .from_oracle_text(ETERNAL_SCOURGE)
        .with_subtypes(vec!["Eldrazi", "Horror"])
        .with_mana_cost(cost(&[], 3))
        .id();
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    let idol = scenario
        .add_artifact_to_hand_from_oracle(P0, "Idol of Endurance", IDOL_OF_ENDURANCE)
        .with_mana_cost(cost(&[ManaCostShard::White], 2))
        .id();
    let lands: Vec<ObjectId> = (0..land_count)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let mut runner = scenario.build();
    runner.cast(idol).resolve();
    for (card, mana_value) in [(scourge, 3), (bears, 2)] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "{card:?} is exiled with the Idol"
        );
        assert_eq!(
            runner.state().objects[&card].mana_cost.mana_value(),
            mana_value,
            "fixture guard: the Idol exiles creature cards with mana value 3 or less"
        );
        assert!(
            runner
                .state()
                .exile_links
                .iter()
                .any(|link: &ExileLink| link.exiled_id == card && link.source_id == idol),
            "{card:?} is linked to the Idol"
        );
    }
    IdolScourgePile {
        runner,
        idol,
        scourge,
        bears,
        lands,
    }
}

/// CR 608.2c + CR 611.2a: Eternal Scourge's own "You may cast this card from
/// exile" is a different route than the Idol's grant, so casting it does not
/// spend the grant's one free creature cast: Grizzly Bears stays castable free.
#[test]
fn idol_of_endurance_grant_survives_a_cast_the_card_could_make_itself() {
    let IdolScourgePile {
        mut runner,
        idol,
        scourge,
        bears,
        lands,
    } = idol_scourge_pile(5);
    let spent = tapped_count(&runner, &lands);
    assert_eq!(spent, 3, "{{2}}{{W}} paid the Idol");

    runner.activate(idol, 0).resolve();
    let after_activation = tapped_count(&runner, &lands);
    assert_eq!(after_activation, spent + 2, "{{1}}{{W}} paid");
    assert!(cast_is_offered(&runner, scourge));
    assert!(cast_is_offered(&runner, bears));

    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        after_activation,
        "the Scourge cast is free"
    );
    assert!(
        cast_is_offered(&runner, bears),
        "the grant's one cast remains available for the Bears"
    );
    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        after_activation,
        "the Bears cast is free too"
    );
}

/// CR 608.2c + CR 611.2a: Grizzly Bears has no permission of its own, so
/// casting it spends the Idol's one free cast. Eternal Scourge stays castable
/// on the card's own route, paying its printed cost.
#[test]
fn idol_of_endurance_grant_spent_by_the_bears_leaves_the_scourges_own_route() {
    let IdolScourgePile {
        mut runner,
        idol,
        scourge,
        bears,
        lands,
    } = idol_scourge_pile(8);
    runner.activate(idol, 0).resolve();
    let spent = tapped_count(&runner, &lands);
    assert_eq!(spent, 5, "the Idol cast and its activation");

    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent,
        "the Bears cast through the grant is free"
    );
    assert!(
        cast_is_offered(&runner, scourge),
        "the Scourge's own permission is a separate route"
    );
    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 3,
        "the Scourge pays its own {{3}}"
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

const BIRCHLORE_RANGERS: &str = "Tap two untapped Elves you control: Add one mana of any color.\n\
     Morph {G} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any \
     time for its morph cost.)";

const PATRON_OF_THE_WILD: &str = "Morph {2}{G} (You may cast this card face down as a 2/2 \
     creature for {3}. Turn it face up any time for its morph cost.)\n\
     When this creature is turned face up, target creature gets +3/+3 until end of turn.";

/// Cast `card` and, when the engine asks for the casting method, elect the
/// face-down alternative (CR 702.37c); then pass priority until it resolves.
fn cast_face_down_to_resolution(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the granted card is cast");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::AlternativeCastChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("elect the face-down alternative");
    }
    assert!(
        runner.state().objects[&card].face_down,
        "reach-guard: the cast is the face-down cast"
    );
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the face-down cast did not resolve");
}

/// A scenario with a Soul-Jar and two real printed morph Elves on the
/// battlefield; the caller kills them with Murder so the Jar's own dies trigger
/// builds the pile.
struct MorphPile {
    runner: GameRunner,
    jar: ObjectId,
    first: ObjectId,
    second: ObjectId,
    key: Option<ObjectId>,
    lands: Vec<ObjectId>,
}

fn morph_pile(with_key: bool, land_count: usize) -> MorphPile {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario
        .add_artifact_from_oracle(P0, "Serpent's Soul-Jar", SERPENTS_SOUL_JAR)
        .with_mana_cost(cost(&[ManaCostShard::Black], 2))
        .id();
    let first = scenario
        .add_creature(P0, "Birchlore Rangers", 1, 1)
        .from_oracle_text(BIRCHLORE_RANGERS)
        .with_subtypes(vec!["Elf", "Druid", "Shaman"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 0))
        .id();
    let second = scenario
        .add_creature(P0, "Patron of the Wild", 1, 1)
        .from_oracle_text(PATRON_OF_THE_WILD)
        .with_subtypes(vec!["Elf", "Shaman"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 0))
        .id();
    let key = with_key.then(|| {
        scenario
            .add_artifact_from_oracle(P0, "Voltaic Key", VOLTAIC_KEY)
            .with_mana_cost(cost(&[], 1))
            .id()
    });
    let murders: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 1))
                .id()
        })
        .collect();
    // Half the lands are green, half blue: whichever {3} the face-down casts
    // pay, a second {3} stays payable afterwards.
    let lands: Vec<ObjectId> = (0..land_count)
        .map(|i| {
            scenario.add_basic_land(
                P0,
                if i % 2 == 0 {
                    ManaColor::Green
                } else {
                    ManaColor::Blue
                },
            )
        })
        .collect();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    for (murder, card) in murders.iter().zip([first, second]) {
        runner.cast(*murder).target_object(card).resolve();
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "the Jar's dies trigger exiled the dead Elf"
        );
    }
    MorphPile {
        runner,
        jar,
        first,
        second,
        key,
        lands,
    }
}

/// CR 608.2c + CR 611.2a + CR 702.37c: the Soul-Jar's one creature-spell cast is
/// spent by a FACE-DOWN cast through it too. The grant is a normal-cost
/// authority that admits the face-down rider (CR 118.9a: the {3} is the cost,
/// the grant only lends zone authority), so the cast must be charged to the
/// grant's shared budget even though the face-down cast elects no cost slot.
#[test]
fn soul_jar_face_down_cast_spends_the_grants_single_cast_budget() {
    let MorphPile {
        mut runner,
        jar,
        first,
        second,
        lands,
        ..
    } = morph_pile(false, 6);
    runner.activate(jar, 0).resolve();
    assert!(cast_is_offered(&runner, first));
    assert!(
        cast_is_offered(&runner, second),
        "reach-guard: the second Elf is castable before the first cast"
    );
    assert_eq!(tapped_count(&runner, &lands), 0);

    cast_face_down_to_resolution(&mut runner, first);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert!(runner.state().objects[&first].face_down);
    assert_eq!(
        tapped_count(&runner, &lands),
        3,
        "the face-down {{3}} is paid, not the grant's {{G}}"
    );
    let untapped = lands.len() - tapped_count(&runner, &lands);
    assert!(
        untapped >= 3,
        "reach-guard: a second face-down {{3}} stays affordable ({untapped} lands untapped)"
    );
    assert!(
        !cast_is_offered(&runner, second),
        "the one cast the grant allows is spent: the second Elf is not castable, face up or down"
    );
    let second_card_id = runner.state().objects[&second].card_id;
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: second,
                card_id: second_card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "dispatching the second cast directly is refused"
    );
    assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
}

/// Two Jar activations on one pile are two grants (two groups): a face-down
/// cast spends exactly one of them (the first in vector order), and the other
/// group's cast remains available for the sibling card.
#[test]
fn soul_jar_two_activations_face_down_spend_one_group_per_cast() {
    let MorphPile {
        mut runner,
        jar,
        first,
        second,
        key,
        lands,
    } = morph_pile(true, 8);
    let key = key.expect("the Key is on the battlefield");
    runner.activate(jar, 0).resolve();
    runner.activate(key, 0).target_object(jar).resolve();
    assert!(
        !runner.state().objects[&jar].tapped,
        "Voltaic Key untapped the Jar"
    );
    runner.activate(jar, 0).resolve();
    assert_eq!(runner.state().exile_play_single_use_consumed.len(), 0);

    cast_face_down_to_resolution(&mut runner, first);
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        1,
        "exactly one activation's budget is spent by the first face-down cast"
    );
    assert!(
        cast_is_offered(&runner, second),
        "the second activation's grant is still unspent"
    );
    cast_face_down_to_resolution(&mut runner, second);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        2,
        "the second face-down cast spends the other activation's budget"
    );
    assert_eq!(
        tapped_count(&runner, &lands),
        1 + 3 + 3,
        "the Key's {{1}} plus one face-down {{3}} per cast"
    );
}
