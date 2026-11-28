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
//! Agatha's Soul Cauldron can grant the Paleontologist's exile ability to
//! Assimilation Assessor. That grant is likewise foreign to the Assessor's
//! printed pair, whether the exile happened before or after the attack latch.
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
//!
//! Assimilation Assessor is reported unsupported
//! (`UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP`); its text is used here only as
//! the vehicle for the CR 607.5 copiable-set distinction on a linked-pile read.

use engine::game::combat::AttackTarget;
use engine::game::game_object::ExileSupplier;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityProvenance, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
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

const CAULDRON: &str =
    "You may spend mana as though it were mana of any color to activate abilities of creatures \
     you control.\n\
     Creatures you control with +1/+1 counters on them have all activated abilities of all \
     creature cards exiled with Agatha's Soul Cauldron.\n\
     {T}: Exile target card from a graveyard. When a creature card is exiled this way, put a \
     +1/+1 counter on target creature you control.";

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

/// Both players pass until the original top entry leaves the stack. Resolution
/// may put a new entry there without reducing the stack's length.
fn resolve_one(runner: &mut GameRunner) {
    let top_id = runner.state().stack.back().expect("a stack entry").id;
    for _ in 0..6 {
        if !runner.state().stack.iter().any(|entry| entry.id == top_id) {
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

/// CR 607.1 + CR 607.2a + CR 607.5: the Assessor's attack reads the pile of
/// its printed ETB ability, not another exile ability it gained in layer 6
/// (CR 613.1f). Before the fix, the granted exile entered the latch as
/// unattributed and opened an EffectZoneChoice alongside the real ETB exile.
#[test]
fn a_granted_exile_latched_before_attack_is_not_assimilated() {
    granted_exile_is_not_assimilated(true);
}

#[test]
fn a_granted_exile_made_while_attack_trigger_waits_is_not_assimilated() {
    granted_exile_is_not_assimilated(false);
}

fn granted_exile_is_not_assimilated(exile_before_attack: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let assessor = scenario
        .add_creature_to_hand_from_oracle(P0, "Assimilation Assessor", 5, 5, ASSESSOR)
        .as_artifact_creature()
        .with_subtypes(vec!["Borg"])
        .with_mana_cost(cost(&[ManaCostShard::Black], 4))
        .id();
    let cauldron = scenario
        .add_artifact_from_oracle(P0, "Agatha's Soul Cauldron", CAULDRON)
        .as_legendary()
        .with_mana_cost(cost(&[], 2))
        .id();
    let paleontologist = scenario
        .add_creature_to_graveyard(P0, "Intrepid Paleontologist", 2, 2)
        .with_subtypes(vec!["Human", "Druid"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .from_oracle_text(PALEONTOLOGIST)
        .id();
    let etb_exile = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    // Give the reflexive trigger two legal counter recipients.
    let other_recipient = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(&[ManaCostShard::Green], 1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Bear".into(), "Borg".into()];
    runner.cast(assessor).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&etb_exile].zone, Zone::Exile);
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.exiled_id == etb_exile && link.source_id == assessor));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    let card_id = CardId(runner.state().next_object_id);
    let granted_exile = create_object(
        runner.state_mut(),
        card_id,
        P1,
        "Runeclaw Bear".into(),
        Zone::Graveyard,
    );
    {
        let bear = runner.state_mut().objects.get_mut(&granted_exile).unwrap();
        bear.card_types.core_types.push(CoreType::Creature);
        bear.card_types.subtypes.push("Bear".into());
        bear.base_card_types = bear.card_types.clone();
        bear.mana_cost = cost(&[ManaCostShard::Green], 1);
        bear.base_mana_cost = bear.mana_cost.clone();
        bear.power = Some(2);
        bear.toughness = Some(2);
        bear.base_power = Some(2);
        bear.base_toughness = Some(2);
    }

    // CR 603.12: the Cauldron exiles the donor first, then the reflexive
    // trigger targets the creature that will receive the counter.
    let cauldron_exile = ability_index(&runner, cauldron, is_exile);
    activate_on_stack(&mut runner, cauldron, cauldron_exile, paleontologist);
    resolve_one(&mut runner);
    assert_eq!(runner.state().objects[&paleontologist].zone, Zone::Exile);
    let WaitingFor::TriggerTargetSelection {
        source_id,
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Cauldron's reflexive trigger must ask for a creature: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(cauldron));
    let offered = &target_slots[selection.current_slot].legal_targets;
    assert!(offered.contains(&TargetRef::Object(assessor)));
    assert!(offered.contains(&TargetRef::Object(other_recipient)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(assessor)),
        })
        .expect("target the Assessor with the Cauldron's reflexive trigger");
    resolve_one(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        runner.state().objects[&assessor]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    let granted_index = ability_index(&runner, assessor, is_exile);
    assert_eq!(
        runner.state().objects[&assessor].activated_ability_provenance(granted_index),
        AbilityProvenance::Granted
    );

    let exile_with_grant = |runner: &mut GameRunner| {
        activate_on_stack(runner, assessor, granted_index, granted_exile);
        resolve_one(runner);
        assert_eq!(runner.state().objects[&granted_exile].zone, Zone::Exile);
        runner
            .state()
            .exile_links
            .iter()
            .find(|link| link.exiled_id == granted_exile && link.source_id == assessor)
            .expect("the granted activation made a link under the same source")
            .supplier
    };

    // Same-turn combat fixture, as in borg_cube_assimilate.rs. No intervening
    // turn is needed to test the timing of the attack's linked-exile latch.
    runner
        .state_mut()
        .objects
        .get_mut(&assessor)
        .unwrap()
        .summoning_sick = false;
    let before_attack = exile_before_attack.then(|| exile_with_grant(&mut runner));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(assessor, AttackTarget::Player(P1))])
        .expect("declare the Assessor as attacker");
    assert_eq!(runner.state().stack.len(), 1, "the attack trigger waits");
    let attack = runner.state().stack.back().unwrap();
    assert_eq!(attack.source_id, assessor);
    let latched = &attack
        .ability()
        .and_then(|ability| ability.trigger_source.as_ref())
        .expect("the attack trigger latched its source")
        .linked_exile_snapshot;
    assert!(latched.iter().any(|snap| snap.exiled_id == etb_exile));
    assert_eq!(
        latched.iter().any(|snap| snap.exiled_id == granted_exile),
        exile_before_attack,
        "the test must place the granted exile on the intended side of the latch"
    );
    let supplier = before_attack.unwrap_or_else(|| exile_with_grant(&mut runner));
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the attack trigger waits"
    );

    // CR 608.2d: two eligible creatures would require a choice. Instead the
    // only linked creature is assimilated and resolution returns to priority.
    resolve_one(&mut runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "a granted exile must not make the printed pile a two-card choice"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    let assimilated = &runner.state().objects[&etb_exile];
    assert_eq!(assimilated.zone, Zone::Battlefield);
    assert_eq!(assimilated.controller, P0);
    assert_eq!(assimilated.counters.get(&CounterType::Plus1Plus1), Some(&1));
    assert_eq!(runner.state().objects[&granted_exile].zone, Zone::Exile);
    assert!(matches!(supplier, Some(ExileSupplier::Granted)));
}

fn is_exile(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        }
    )
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

const BORG_CUBE: &str = "Affinity for artifact creatures\n\
     Whenever this Spacecraft enters or attacks, assimilate a creature card exiled with it. \
     If you can't, exile up to one target creature. (To assimilate a card, put it onto the \
     battlefield under your control with a +1/+1 counter. It's a Borg artifact creature and \
     loses all other creature types.)\n\
     Station (Tap another creature you control: Put charge counters equal to its power on this \
     Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n\
     12+ | Artifact creatures you control have flying.";

/// CR 607.5 (the maintainer's scenario): Volrath copies a stationed Borg Cube
/// ("it has all printed abilities, including the station symbols"; counters are
/// not copied, so Volrath carries its own twelve charge counters) and attacks.
/// The Cube's attack trigger, linked to the Cube-copy ability set, waits on the
/// stack with an empty pile. In response Volrath becomes a copy of Intrepid
/// Paleontologist and exiles a creature card from a graveyard with ITS ability,
/// which is not linked to the Cube trigger's ability. The trigger's "assimilate a
/// creature card exiled with it" finds nothing, so "If you can't" runs and exiles
/// the trigger's chosen target instead; the graveyard card is not assimilated.
///
/// Positive reach guard for the negative: the fallback exile of the chosen
/// creature happens (the trigger resolved and found its pile empty), and the
/// graveyard card is in exile linked to Volrath by the Paleontologist's ability.
///
/// Mutant: drop the pair filters of `players::pile_entries` (admit a card exiled
/// by another copiable set): the card is assimilated, the bystander stays put.
#[test]
fn volrath_as_borg_cube_does_not_assimilate_a_card_exiled_by_another_copiable_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
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
    // The stationed Borg Cube Volrath copies (a creature with a counter on it).
    let cube = {
        let mut b = scenario.add_creature_from_oracle(P0, "Borg Cube", 6, 6, "");
        b.as_artifact()
            .with_subtypes(vec!["Spacecraft"])
            .with_mana_cost(cost(&[ManaCostShard::White, ManaCostShard::Black], 5))
            .from_oracle_text_with_keywords(&["Affinity", "Station"], BORG_CUBE);
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
    let bystander = scenario.add_creature(P1, "Test Bystander", 1, 1).id();
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
    // Station both: the Cube is a creature with counters (Volrath's copy target),
    // and Volrath's own charge counters (not copied) make its Cube copy a creature.
    for id in [cube, volrath] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 12);
    }
    runner
        .state_mut()
        .objects
        .get_mut(&volrath)
        .unwrap()
        .summoning_sick = false;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Volrath becomes a copy of the stationed Borg Cube.
    let copy_ability = ability_index(&runner, volrath, |e| matches!(e, Effect::BecomeCopy { .. }));
    activate_on_stack(&mut runner, volrath, copy_ability, cube);
    resolve_one(&mut runner);
    assert_eq!(runner.state().objects[&volrath].name, "Borg Cube");

    // Attack: the copied attack trigger goes on the stack with an empty pile; its
    // "up to one target creature" is the bystander.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(volrath, AttackTarget::Player(P1))])
        .expect("declare the Borg Cube copy as attacker");
    while let WaitingFor::TriggerTargetSelection { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bystander)),
            })
            .expect("choose the fallback target");
    }
    assert_eq!(runner.state().stack.len(), 1, "the attack trigger waits");
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == volrath),
        "reach: nothing is exiled with Volrath yet"
    );

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
        "reach: the exile is linked to Volrath by the Paleontologist ability"
    );

    // The Cube trigger resolves: its pair exiled nothing, so "If you can't" runs.
    resolve_one(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "settled: {:?} {:?}",
        runner.state().waiting_for,
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects[&bystander].zone,
        Zone::Exile,
        "reach: the trigger found no card of its own pair and took its fallback"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "a card exiled by another ability pair is not assimilated"
    );
}
