//! "Until end of turn, you may cast a [creature] spell from among cards exiled
//! with this artifact" on a NORMAL-COST grant (Serpent's Soul-Jar) authorizes
//! ONE cast in all (CR 608.2c: "a spell" is one cast), across every card of the
//! linked pile, not one per card. The permissions one resolution writes share a
//! budget (`ExileWithAltCost::single_use_group`); the first cast spends it and
//! the rest of the pile is no longer castable through the grant.
//!
//! A modal card's BACK-face cast through such a grant spends none of the
//! budget: the grant's "creature spell" restriction was evaluated on the card's
//! front face when the permission was attached (CR 712.11c), so the back-face
//! cast is not the one cast the grant offers. This pins main's behaviour for
//! that path, where no budget existed at all; admitting the back face through
//! the grant in the first place is a pre-existing gap.
//!
//! A FREE grant (Idol of Endurance, "without paying its mana cost") carries no
//! shared budget: every card in its pile stays castable through the grant after
//! the first cast. This is UNCHANGED FROM MAIN, not a rules-correct cap: the
//! engine cannot offer the player election between the free grant and a pile
//! card's own cast-from-exile permission, so charging a shared group there
//! would strip the rest of the pile. The printed cap of one cast
//! (CR 608.2c: "a spell" is one cast) therefore stays unenforced for a free
//! grant. These witnesses pin main's observable behaviour, not its correctness.
//!
//! A NORMAL-COST grant gets the same carve-out for the pile card that carries
//! its own "you may cast this card from exile" permission (Eternal Scourge):
//! the engine cannot distinguish a Jar cast of that card from its own
//! permission, so the cast spends none of the shared budget and the grant
//! stays available for the rest of the pile. A Jar cast of the Scourge
//! followed by one of Grizzly Bears therefore both happen, exactly as
//! on main. Pinned as main-compatible, not as rules-ideal.
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
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::CastingPermission;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, ExileLink, ExileLinkKind, WaitingFor};
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
/// when it enters; its free activation grant is per-object and uncapped,
/// unchanged from main, so every card in the pile is castable for free before
/// and after the first cast. Witness, not a correctness claim: the free grant
/// stays per-object because the engine offers no election between it and a pile
/// card's own cast-from-exile permission; see the module doc.
#[test]
fn idol_of_endurance_free_grant_is_per_object_unchanged_from_main() {
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

    // The free cast spends no mana and does not close the grant.
    cast_to_resolution(&mut runner, second);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &lands), spent + 2, "the cast is free");
    assert!(
        cast_is_offered(&runner, first),
        "unchanged from main: the free per-object grant is not capped, so the other card is still offered"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Exile);

    cast_to_resolution(&mut runner, first);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 2,
        "the second cast is free too"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ) && runner.state().stack.is_empty()
    );
}

/// Two activations of the Idol before any cast: each free grant is per-object
/// and uncapped, unchanged from main, so both pile cards stay castable for free
/// (the second activation adds no second cap). Voltaic Key's printed
/// "{1}, {T}: Untap target artifact." is what frees the Idol for the second
/// activation. Witness, not a correctness claim: the free grant is per-object
/// because the engine offers no election between it and a pile card's own
/// cast-from-exile permission.
#[test]
fn idol_of_endurance_two_activations_leave_the_free_grant_uncapped_as_on_main() {
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
        "unchanged from main: the free per-object grant is not capped, so the other card is still offered"
    );
    cast_to_resolution(&mut runner, first);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &lands), spent, "both casts were free");
}

/// Idol of Endurance on the battlefield, with Eternal Scourge and Grizzly Bears
/// exiled by its ETB trigger. Returns the runner, the Idol, the two pile cards,
/// and eight white lands.
fn idol_scourge_pile() -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scourge = scenario
        .add_creature_to_graveyard(P0, "Eternal Scourge", 3, 3)
        .with_subtypes(vec!["Eldrazi", "Horror"])
        .with_mana_cost(cost(&[], 3))
        .from_oracle_text(ETERNAL_SCOURGE)
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
    let lands: Vec<ObjectId> = (0..8)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let mut runner = scenario.build();
    runner.cast(idol).resolve();
    for card in [scourge, bears] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "the Idol's ETB exiles the pile"
        );
    }
    (runner, idol, scourge, bears, lands)
}

/// Eternal Scourge's own "you may cast this card from exile" permission and the
/// Idol's free grant both authorize the same card. Casting the Scourge free
/// through the grant spends no shared budget (the free grant is per-object and
/// uncapped, unchanged from main), so the other pile card stays castable for
/// free. Witness, not a correctness claim: the engine offers no election
/// between the free grant and the Scourge's own cast-from-exile permission, so
/// the grant carries no shared budget.
#[test]
fn idol_of_endurance_free_cast_spends_no_shared_slot_unchanged_from_main() {
    let (mut runner, idol, scourge, bears, lands) = idol_scourge_pile();
    let spent = tapped_count(&runner, &lands);
    assert_eq!(spent, 3, "{{2}}{{W}} paid the Idol");
    runner.activate(idol, 0).resolve();
    assert_eq!(tapped_count(&runner, &lands), spent + 2, "{{1}}{{W}} paid");
    assert!(cast_is_offered(&runner, scourge));
    assert!(cast_is_offered(&runner, bears));

    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 2,
        "the Scourge is cast free, not for its own {{3}}"
    );
    assert!(
        cast_is_offered(&runner, bears),
        "unchanged from main: the free per-object grant is not capped, so the Bears stay offered after the Scourge's cast"
    );
    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 2,
        "the Bears are cast free too"
    );
}

/// Casting the Bears first leaves the Scourge castable for free: the free grant
/// is per-object and uncapped, unchanged from main, so it survives the first
/// cast, and the Scourge's own route remains as well. Witness, not a correctness
/// claim: the free grant is per-object, so no cast spends a shared budget.
#[test]
fn idol_of_endurance_bears_first_keeps_the_scourge_castable_unchanged_from_main() {
    let (mut runner, idol, scourge, bears, lands) = idol_scourge_pile();
    let spent = tapped_count(&runner, &lands);
    runner.activate(idol, 0).resolve();
    assert_eq!(tapped_count(&runner, &lands), spent + 2, "{{1}}{{W}} paid");

    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert!(
        cast_is_offered(&runner, scourge),
        "the Scourge is still offered after the Bears' free cast"
    );
    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &lands),
        spent + 2,
        "both casts are free: the Scourge's own route would have cost {{3}}"
    );
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

/// A normal-cost grant keeps the shared budget. Serpent's Soul-Jar's cast grant
/// is not restricted to Elves (only its dies trigger is), so a pile of Eternal
/// Scourge and Grizzly Bears stages the control: casting the Bears through the
/// grant spends it, after which the Jar's grant no longer authorizes the
/// Scourge. The Scourge stays offered by its own "you may cast this card from
/// exile" route.
#[test]
fn soul_jar_normal_cost_grant_spends_its_budget_leaving_only_the_own_route() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario
        .add_artifact_from_oracle(P0, "Serpent's Soul-Jar", SERPENTS_SOUL_JAR)
        .with_mana_cost(cost(&[ManaCostShard::Black], 2))
        .id();
    let scourge = scenario
        .add_creature_to_exile(P0, "Eternal Scourge", 3, 3)
        .with_subtypes(vec!["Eldrazi", "Horror"])
        .with_mana_cost(cost(&[], 3))
        .from_oracle_text(ETERNAL_SCOURGE)
        .id();
    let bears = scenario
        .add_creature_to_exile(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    let forests: Vec<ObjectId> = (0..6)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    for card in [scourge, bears] {
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: card,
            source_id: jar,
            kind: ExileLinkKind::TrackedBySource,
            supplier: None,
        });
    }

    runner.activate(jar, 0).resolve();
    assert!(cast_is_offered(&runner, scourge));
    assert!(cast_is_offered(&runner, bears));

    // The Bears have no route of their own, so this cast is the Jar's grant's,
    // paying the printed {1}{G} and spending the grant's single shared cast.
    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &forests),
        2,
        "the grant pays the printed {{1}}{{G}}"
    );
    assert!(
        !runner.state().objects[&scourge]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::ExileWithAltCost {
                    single_use_group: Some(_),
                    ..
                }
            )),
        "the spent grant is stripped from the Scourge"
    );
    assert!(
        cast_is_offered(&runner, scourge),
        "the Scourge's own 'you may cast this card from exile' route still offers it"
    );
}

/// Serpent's Soul-Jar on the battlefield with Eternal Scourge (own
/// "you may cast this card from exile" permission) and Grizzly Bears exiled
/// with it. Returns the runner, the Jar, the two pile cards, and six green
/// lands.
fn soul_jar_scourge_pile() -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario
        .add_artifact_from_oracle(P0, "Serpent's Soul-Jar", SERPENTS_SOUL_JAR)
        .with_mana_cost(cost(&[ManaCostShard::Black], 2))
        .id();
    let scourge = scenario
        .add_creature_to_exile(P0, "Eternal Scourge", 3, 3)
        .with_subtypes(vec!["Eldrazi", "Horror"])
        .with_mana_cost(cost(&[], 3))
        .from_oracle_text(ETERNAL_SCOURGE)
        .id();
    let bears = scenario
        .add_creature_to_exile(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    let forests: Vec<ObjectId> = (0..6)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    for card in [scourge, bears] {
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: card,
            source_id: jar,
            kind: ExileLinkKind::TrackedBySource,
            supplier: None,
        });
    }
    (runner, jar, scourge, bears, forests)
}

/// Serpent's Soul-Jar's normal-cost grant is shared, but the own-permission
/// carve-out matches main: the engine auto-elects the Jar grant for Eternal
/// Scourge and cannot distinguish that cast from the Scourge's own
/// "you may cast this card from exile" route, so the cast spends no shared
/// budget and the same Jar activation still authorizes Grizzly Bears. A Jar
/// cast of the Scourge plus a Jar cast of the Bears is therefore exactly main's
/// two-cast outcome — a compatibility witness, not a rules-ideal reading: the
/// engine cannot tell a Jar cast of the Scourge from the Scourge's own
/// cast-from-exile route.
#[test]
fn soul_jar_scourge_first_keeps_the_bears_castable_through_the_grant() {
    let (mut runner, jar, scourge, bears, forests) = soul_jar_scourge_pile();
    runner.activate(jar, 0).resolve();
    assert!(cast_is_offered(&runner, scourge));
    assert!(cast_is_offered(&runner, bears));

    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &forests), 3, "the Scourge's {{3}}");
    assert!(
        runner.state().exile_play_single_use_consumed.is_empty(),
        "the carve-out spends no shared budget for the own-permission card"
    );
    assert!(
        cast_is_offered(&runner, bears),
        "same as main: the Jar activation still authorizes the rest of the pile"
    );

    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &forests),
        5,
        "the Bears' printed {{1}}{{G}} is paid through the grant"
    );
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        1,
        "the Bears' cast is the one cast the grant allows"
    );
}

/// The control: a Bears cast first is unambiguously the Jar's grant, so it
/// spends the shared budget; Eternal Scourge stays castable through its own
/// "you may cast this card from exile" route and is cast for its printed {3}.
/// The two casts (Bears through the grant, Scourge through its own route) are
/// main's observable behaviour, not a rules-ideal claim: the engine offers no
/// election between the two routes.
#[test]
fn soul_jar_bears_first_spends_the_grant_but_the_scourge_keeps_its_own_route() {
    let (mut runner, jar, scourge, bears, forests) = soul_jar_scourge_pile();
    runner.activate(jar, 0).resolve();

    cast_to_resolution(&mut runner, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &forests), 2, "the Bears' {{1}}{{G}}");
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        1,
        "the Bears' cast spends the Jar's one cast"
    );
    assert!(
        cast_is_offered(&runner, scourge),
        "the Scourge's own route still offers it"
    );

    cast_to_resolution(&mut runner, scourge);
    assert_eq!(runner.state().objects[&scourge].zone, Zone::Battlefield);
    assert_eq!(
        tapped_count(&runner, &forests),
        5,
        "the Scourge pays its own printed {{3}}"
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

/// The noncreature spell face of the synthetic modal DFC below. CR 712.11c:
/// only the elected face is evaluated as the spell, so this face's
/// characteristics — not the front creature's — are the ones the cast presents.
fn noncreature_modal_back_face() -> BackFaceData {
    let mut card_types = CardType::default();
    card_types.core_types.push(CoreType::Enchantment);
    BackFaceData {
        name: "Bridge Face".to_string(),
        card_types,
        mana_cost: cost(&[ManaCostShard::Green], 1),
        color: vec![ManaColor::Green],
        layout_kind: Some(LayoutKind::Modal),
        ..BackFaceData::default()
    }
}

/// Serpent's Soul-Jar plus the pile its own dies trigger builds: a synthetic
/// modal DFC whose front face is an Elf creature and whose back face is a
/// noncreature spell, and a plain Elf creature. Both die to printed Murder, so
/// the Jar's trigger — not a fixture — creates the linked exile the grant
/// reads. Returns the runner, the Jar, the modal card, and the sibling.
fn soul_jar_modal_pile() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario
        .add_artifact_from_oracle(P0, "Serpent's Soul-Jar", SERPENTS_SOUL_JAR)
        .with_mana_cost(cost(&[ManaCostShard::Black], 2))
        .id();
    // Front face: an Elf creature, so the Jar's own dies trigger builds the pile.
    let mdfc = scenario
        .add_creature(P0, "MDFC Elf", 1, 4)
        .with_subtypes(vec!["Elf"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 0))
        .id();
    let sibling = scenario
        .add_creature(P0, "Elvish Warrior", 2, 3)
        .with_subtypes(vec!["Elf", "Warrior"])
        .with_mana_cost(cost(&[ManaCostShard::Green, ManaCostShard::Green], 0))
        .id();
    let murders: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 1))
                .id()
        })
        .collect();
    // Six green sources: the back face's {1}{G} and the sibling's {G}{G}.
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    // {1}{B}{B} twice: six black mana pays for both Murders in this main phase.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    // Fixture: the modal card's back face (a noncreature spell). The front face
    // stays the live face in exile (CR 712.8a), which is what receives the
    // grant.
    runner
        .state_mut()
        .objects
        .get_mut(&mdfc)
        .expect("the modal DFC exists")
        .back_face = Some(noncreature_modal_back_face());

    for (murder, card) in murders.iter().zip([mdfc, sibling]) {
        runner.cast(*murder).target_object(card).resolve();
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "the Jar's dies trigger exiled the dead Elf"
        );
    }
    (runner, jar, mdfc, sibling)
}

/// CR 712.11c + CR 712.8a + CR 608.2c: the grant's "creature spell" restriction
/// was evaluated on the modal card's front face when the Jar stamped the
/// permission (the only face a modal card has in exile), and the elected back
/// face is never tested against it. A back-face cast through the grant is
/// therefore not the one cast the grant offers: it must not spend the shared
/// budget, and the sibling creature cast the grant does offer stays available.
/// This is main's observable behaviour for the path — main carried no budget at
/// all — not a rules-ideal reading: admitting the back face through the grant
/// in the first place is a pre-existing gap.
#[test]
fn soul_jar_back_face_cast_spends_no_budget_so_the_sibling_creature_stays_castable() {
    let (mut runner, jar, mdfc, sibling) = soul_jar_modal_pile();
    runner.activate(jar, 0).resolve();
    assert!(
        cast_is_offered(&runner, mdfc),
        "reach-guard: the modal DFC's creature front face is inside the grant's pile"
    );
    assert!(cast_is_offered(&runner, sibling));

    // Cast the modal card's BACK face (a noncreature spell) through the grant.
    runner.cast(mdfc).modal_back_face(true).resolve();
    assert_eq!(runner.state().objects[&mdfc].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&mdfc].name,
        "Bridge Face",
        "reach-guard: the cast presented the back face"
    );
    assert!(
        runner.state().exile_play_single_use_consumed.is_empty(),
        "main's behaviour: the back-face cast charges none of the grant's shared budget"
    );

    assert!(
        cast_is_offered(&runner, sibling),
        "the sibling creature cast the grant does offer is still available"
    );
    cast_to_resolution(&mut runner, sibling);
    assert_eq!(runner.state().objects[&sibling].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        1,
        "the sibling's creature cast is the one cast the grant allows"
    );
}

/// The control for the carve-out: the modal card's FRONT face is the creature
/// the grant offers, so electing it spends the grant's one cast. The rule keys
/// on the elected back face, never on `cast_face_committed` (a front election
/// sets that flag too) or on the card's layout. CR 712.11c: the front creature
/// face is the spell evaluated; CR 608.2c: that cast is the grant's one spell.
#[test]
fn soul_jar_front_face_cast_of_the_modal_card_still_spends_the_budget() {
    let (mut runner, jar, mdfc, sibling) = soul_jar_modal_pile();
    runner.activate(jar, 0).resolve();
    assert!(cast_is_offered(&runner, mdfc));
    assert!(cast_is_offered(&runner, sibling));

    runner.cast(mdfc).modal_back_face(false).resolve();
    assert_eq!(runner.state().objects[&mdfc].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&mdfc].name,
        "MDFC Elf",
        "reach-guard: the cast presented the front face"
    );
    assert_eq!(
        runner.state().exile_play_single_use_consumed.len(),
        1,
        "the front-face creature cast is the one cast the grant offers"
    );
    assert!(
        !cast_is_offered(&runner, sibling),
        "the one cast the grant allows is spent: the sibling is not castable"
    );
}
