//! Corpse Appraiser — ETB exile from a graveyard, then dig if a card was
//! put into exile this way.
//!
//! Oracle: "When this creature enters, exile up to one target creature card
//! from a graveyard. If a card is put into exile this way, look at the top
//! three cards of your library, then put one of those cards into your hand
//! and the rest into your graveyard."
//!
//! Also covers Mysterious Limousine, the other card whose rider the shared
//! "put into exile this way" grammar now conditions.

use engine::game::combat::AttackTarget;
use engine::game::scenario::GameRunner;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CORPSE_APPRAISER: &str = "When this creature enters, exile up to one target creature card from a graveyard. If a card is put into exile this way, look at the top three cards of your library, then put one of those cards into your hand and the rest into your graveyard.";

fn floating_ubr() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
    ]
}

/// SHAPE: the leading-if "put into exile this way" peels so the look-at +
/// put-one-keep-rest chain can assemble as a `Dig` gated by
/// `ZoneChangedThisWay { destination: Exile }`.
#[test]
fn corpse_appraiser_etb_parses_exile_then_conditional_dig() {
    let parsed = parse_oracle_text(CORPSE_APPRAISER, "Corpse Appraiser", &[], &[], &[]);
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        ),
        "first instruction is exile from a graveyard, got {:?}",
        execute.effect
    );
    let dig = execute.sub_ability.as_ref().expect("dig sub_ability");
    let Effect::Dig {
        count,
        keep_count,
        destination,
        rest_destination,
        ..
    } = &*dig.effect
    else {
        panic!("expected Dig follow-up, got {:?}", dig.effect);
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
    assert_eq!(*keep_count, Some(1));
    assert_eq!(*destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination: this_way_dest,
    }) = &dig.condition
    else {
        panic!(
            "Dig must be gated by ZoneChangedThisWay, got {:?}",
            dig.condition
        );
    };
    assert_eq!(*this_way_dest, Some(Zone::Exile));
    match filter {
        engine::types::ability::TargetFilter::Typed(typed) => {
            assert_eq!(typed.type_filters, vec![TypeFilter::Card]);
        }
        other => panic!("expected Typed Card this-way filter, got {other:?}"),
    }
}

/// CR 608.2c + CR 701.13a + CR 701.20e: exiling a graveyard creature card
/// makes the "put into exile this way" gate true, so the look-at-three
/// instruction surfaces a DigChoice (keep one, rest to graveyard).
#[test]
fn corpse_appraiser_etb_exile_surfaces_dig_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let appraiser = scenario
        .add_creature_to_hand_from_oracle(P0, "Corpse Appraiser", 3, 3, CORPSE_APPRAISER)
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P1, "Doomed Bear", 2, 2)
        .id();
    let mill_two = scenario.add_card_to_library_top(P0, "Mill Two");
    let mill_one = scenario.add_card_to_library_top(P0, "Mill One");
    let keep = scenario.add_card_to_library_top(P0, "Keep Me");
    scenario.with_mana_pool(P0, floating_ubr());

    let mut runner = scenario.build();
    let outcome = runner.cast(appraiser).target_object(gy_creature).resolve();

    assert_eq!(
        outcome.zone_of(gy_creature),
        Zone::Exile,
        "the targeted graveyard creature card must be exiled"
    );

    let WaitingFor::DigChoice {
        cards,
        keep_count,
        rest_destination,
        kept_destination,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected DigChoice after a card is put into exile this way, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*keep_count, 1);
    assert_eq!(*kept_destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    assert_eq!(cards.len(), 3, "look at the top three cards");
    assert!(
        cards.contains(&keep) && cards.contains(&mill_one) && cards.contains(&mill_two),
        "DigChoice must offer the three looked-at library cards, got {cards:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("DigChoice SelectCards must succeed");

    let state = runner.state();
    assert_eq!(
        state.objects[&keep].zone,
        Zone::Hand,
        "the chosen looked-at card goes to hand"
    );
    assert_eq!(
        state.objects[&mill_one].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
    assert_eq!(
        state.objects[&mill_two].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
}

/// CR 608.2c: with no creature card in any graveyard the optional "up to one"
/// target is left empty, nothing is put into exile, the parent event ledger is
/// empty, and the "put into exile this way" Dig must neither prompt nor move a
/// library card.
#[test]
fn corpse_appraiser_no_graveyard_creature_does_not_dig() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let appraiser = scenario
        .add_creature_to_hand_from_oracle(P0, "Corpse Appraiser", 3, 3, CORPSE_APPRAISER)
        .id();
    let library = [
        scenario.add_card_to_library_top(P0, "Library Three"),
        scenario.add_card_to_library_top(P0, "Library Two"),
        scenario.add_card_to_library_top(P0, "Library One"),
    ];
    scenario.with_mana_pool(P0, floating_ubr());

    let mut runner = scenario.build();
    let outcome = runner.cast(appraiser).resolve();

    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::DigChoice { .. }),
        "no card was put into exile, so no DigChoice, got {:?}",
        outcome.final_waiting_for()
    );
    for id in library {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Library,
            "no library card may move"
        );
    }
    assert_eq!(runner.state().players[0].graveyard.len(), 0);
}

const MYSTERIOUS_LIMOUSINE: &str = "Whenever this Vehicle enters or attacks, exile up to one other target creature until this Vehicle leaves the battlefield. If a creature is put into exile this way, return each other card exiled with this Vehicle to the battlefield under its owner's control.\nCrew 2";

/// SHAPE: the same grammar conditions Mysterious Limousine's return-each-other
/// continuation on "a creature put into exile this way" (it was unconditioned
/// before the "put into exile" arm existed).
#[test]
fn mysterious_limousine_return_is_gated_by_creature_put_into_exile() {
    let parsed = parse_oracle_text(
        MYSTERIOUS_LIMOUSINE,
        "Mysterious Limousine",
        &[],
        &["Artifact".to_string(), "Vehicle".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::EntersOrAttacks)
        .expect("enters-or-attacks trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "first instruction exiles the target creature, got {:?}",
        execute.effect
    );
    let ret = execute.sub_ability.as_ref().expect("return sub_ability");
    assert!(
        matches!(
            ret.effect.as_ref(),
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                ..
            }
        ),
        "second instruction returns the other exiled cards, got {:?}",
        ret.effect
    );
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination,
    }) = &ret.condition
    else {
        panic!(
            "return must be gated by ZoneChangedThisWay, got {:?}",
            ret.condition
        );
    };
    assert_eq!(*destination, Some(Zone::Exile));
    match filter {
        TargetFilter::Typed(typed) => {
            assert_eq!(typed.type_filters, vec![TypeFilter::Creature]);
        }
        other => panic!("expected Typed Creature this-way filter, got {other:?}"),
    }
}

/// Stage Mysterious Limousine on P0's battlefield with a card already exiled
/// with it (SYNTHETIC: linked as a prior activation's exile would be,
/// CR 607.2a; the link is added after build because a Vehicle that changed
/// zones would lose it, CR 400.7), a crewer, and an opposing creature. Crew it,
/// attack, and answer the attack trigger's optional target with `choice`.
/// Returns (runner, prior exiled card, opposing creature).
fn limousine_attack(choose_opposing: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let limo = scenario
        .add_creature(P0, "Mysterious Limousine", 4, 4)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(MYSTERIOUS_LIMOUSINE)
        .id();
    let crewer = scenario.add_creature(P0, "Crewer", 2, 2).id();
    let prior = scenario
        .add_creature_to_exile(P1, "Previously Exiled Bear", 2, 2)
        .id();
    let victim = scenario.add_creature(P1, "Doomed Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: prior,
        source_id: limo,
        kind: ExileLinkKind::TrackedBySource,
        supplier: None,
    });

    // CR 702.122a: crew 2 with the 2/2.
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: limo,
            creature_ids: vec![],
        })
        .expect("enter crew selection");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: limo,
            creature_ids: vec![crewer],
        })
        .expect("pay the crew cost");
    runner.advance_until_stack_empty();
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(limo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the Limousine attacks");

    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(legal.contains(&TargetRef::Object(victim)));
                GameAction::ChooseTarget {
                    target: choose_opposing.then_some(TargetRef::Object(victim)),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the attack trigger resolves");
    }
    assert!(runner.state().stack.is_empty(), "reach: trigger resolved");
    (runner, prior, victim)
}

/// CR 608.2c + CR 607.2a: a creature really is put into exile, so the
/// continuation runs: the previously exiled card returns, the new victim stays.
#[test]
fn mysterious_limousine_exiling_a_creature_returns_the_other_exiled_card() {
    let (runner, prior, victim) = limousine_attack(true);
    let state = runner.state();
    assert_eq!(state.objects[&victim].zone, Zone::Exile, "victim is exiled");
    assert_eq!(
        state.objects[&prior].zone,
        Zone::Battlefield,
        "the other card exiled with the Vehicle returns"
    );
}

/// CR 608.2c: the optional target is declined, so no creature is put into
/// exile, the gate is false and the previously exiled card must stay exiled.
/// At the merge base this continuation was unconditioned and returned it
/// anyway.
#[test]
fn mysterious_limousine_declined_target_returns_nothing() {
    let (runner, prior, victim) = limousine_attack(false);
    let state = runner.state();
    assert_eq!(state.objects[&victim].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&prior].zone,
        Zone::Exile,
        "nothing was put into exile this way, so nothing returns"
    );
}

/// CR 702.122a: crew `vehicle` by tapping `crewer`.
fn crew(runner: &mut GameRunner, vehicle: ObjectId, crewer: ObjectId) {
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("enter crew selection");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![crewer],
        })
        .expect("pay the crew cost");
    runner.advance_until_stack_empty();
}

/// CR 400.7 + CR 608.2c + CR 702.122a: the "creature" put into exile this way
/// is a crewed Vehicle, which stops being a creature the moment it leaves the
/// battlefield. The noun is read as it was when moved, so the gate still holds
/// and the card the first Limousine exiled earlier (through its own enters
/// trigger) comes back while the crewed Vehicle stays exiled. Both Vehicles
/// are printed Mysterious Limousines (Crew 2); the second is the victim.
#[test]
fn mysterious_limousine_exiling_a_crewed_vehicle_returns_the_other_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let limo = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mysterious Limousine", MYSTERIOUS_LIMOUSINE)
        .with_subtypes(vec!["Vehicle"])
        .id();
    let second_limo = scenario
        .add_creature(P0, "Mysterious Limousine", 4, 4)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(MYSTERIOUS_LIMOUSINE)
        .id();
    let limo_crewer = scenario.add_creature(P0, "Limousine Crewer", 2, 2).id();
    let second_crewer = scenario.add_creature(P0, "Second Crewer", 2, 2).id();
    let first_victim = scenario.add_creature(P1, "First Bear", 2, 2).id();
    // Both players must be able to draw through the turn cycle.
    for n in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {n}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {n}"));
    }
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    // Enters trigger: the first exile happens through gameplay and is linked.
    runner.cast(limo).target_object(first_victim).resolve();
    assert_eq!(runner.state().objects[&first_victim].zone, Zone::Exile);

    // Cycle to P0's next precombat main, attacking and blocking with nothing.
    let start = runner.state().turn_number;
    for _ in 0..600 {
        let st = runner.state();
        if st.turn_number >= start + 2 && st.active_player == P0 && st.phase == Phase::PreCombatMain
        {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the turn cycle advances");
    }
    assert_eq!(runner.state().active_player, P0, "reach: P0's next turn");
    assert_eq!(runner.state().phase, Phase::PreCombatMain);

    crew(&mut runner, limo, limo_crewer);
    crew(&mut runner, second_limo, second_crewer);
    assert!(
        runner.state().objects[&second_limo]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "reach: the crewed Vehicle is a creature on the battlefield"
    );

    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(limo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the Limousine attacks");
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(legal.contains(&TargetRef::Object(second_limo)));
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(second_limo)),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the attack trigger resolves");
    }
    assert!(runner.state().stack.is_empty(), "reach: trigger resolved");

    let state = runner.state();
    assert_eq!(
        state.objects[&second_limo].zone,
        Zone::Exile,
        "the crewed Vehicle was put into exile"
    );
    assert_eq!(
        state.objects[&first_victim].zone,
        Zone::Battlefield,
        "a creature was put into exile this way, so the other exiled card returns"
    );
}

const MURDER: &str = "Destroy target creature.";
const DEATHGORGE_SCAVENGER: &str = "Whenever this creature enters or attacks, you may exile target card from a graveyard. If a creature card is exiled this way, you gain 2 life. If a noncreature card is exiled this way, this creature gets +1/+1 until end of turn.";

/// CR 400.7 + CR 608.2c: the exit last-known information that answers "a
/// creature card is exiled this way" must belong to the move being resolved. A
/// crewed Mysterious Limousine destroyed by Murder leaves a creature-typed exit
/// snapshot, but the card is a noncreature artifact in the graveyard; exiling it
/// from the graveyard with Deathgorge Scavenger is a different move, so the
/// creature-card rider must not gain life.
#[test]
fn deathgorge_scavenger_stale_exit_snapshot_does_not_count_as_a_creature_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let limo = scenario
        .add_creature(P0, "Mysterious Limousine", 4, 4)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(MYSTERIOUS_LIMOUSINE)
        .id();
    let crewer = scenario.add_creature(P0, "Crewer", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let scavenger = scenario
        .add_creature_to_hand_from_oracle(P0, "Deathgorge Scavenger", 3, 2, DEATHGORGE_SCAVENGER)
        .id();
    let mut pool: Vec<ManaUnit> = (0..3)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect();
    pool.extend((0..3).map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    crew(&mut runner, limo, crewer);
    runner.cast(murder).target_object(limo).resolve();
    assert_eq!(
        runner.state().objects[&limo].zone,
        Zone::Graveyard,
        "reach: the crewed Vehicle was destroyed"
    );

    let life_before = runner.state().players[0].life;
    let outcome = runner
        .cast(scavenger)
        .target_object(limo)
        .accept_optional()
        .resolve();
    let _ = outcome;
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&limo].zone,
        Zone::Exile,
        "reach: the noncreature card was exiled from the graveyard"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "a noncreature card was exiled this way: no creature-card life gain"
    );
}

const COME_BACK_WRONG: &str = "Destroy target creature. If a creature card is put into a graveyard this way, return it to the battlefield under your control. Sacrifice it at the beginning of your next end step.";

fn come_back_wrong_scenario(crewed_vehicle: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Come Back Wrong", false, COME_BACK_WRONG)
        .id();
    let victim = if crewed_vehicle {
        scenario
            .add_creature(P0, "Mysterious Limousine", 4, 4)
            .as_artifact()
            .with_subtypes(vec!["Vehicle"])
            .from_oracle_text(MYSTERIOUS_LIMOUSINE)
            .id()
    } else {
        scenario.add_creature(P1, "Doomed Bear", 2, 2).id()
    };
    let crewer = crewed_vehicle.then(|| scenario.add_creature(P0, "Crewer", 2, 2).id());
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    if let Some(crewer) = crewer {
        crew(&mut runner, victim, crewer);
    }
    runner.cast(spell).target_object(victim).resolve();
    (runner, spell, victim)
}

/// CR 109.2a + CR 608.2c: "a creature card is put into a graveyard this way"
/// describes a card in that zone. A crewed Vehicle destroyed by Come Back Wrong
/// is a noncreature artifact card in the graveyard (the temporary creature type
/// is gone), so it must stay there; the exile-arrival last-known reading that
/// Mysterious Limousine needs must not leak into graveyard-destination riders.
#[test]
fn come_back_wrong_leaves_a_destroyed_crewed_vehicle_in_the_graveyard() {
    let (runner, _spell, vehicle) = come_back_wrong_scenario(true);
    assert_eq!(
        runner.state().objects[&vehicle].zone,
        Zone::Graveyard,
        "a noncreature card was put into a graveyard: no return"
    );
}

/// Positive witness for the sibling above: an actual creature card is returned.
#[test]
fn come_back_wrong_returns_a_destroyed_creature_card() {
    let (runner, _spell, bear) = come_back_wrong_scenario(false);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "a creature card was put into a graveyard this way: it returns"
    );
}

/// Verbatim Oracle text of Mimic Vat (Scryfall).
const MIMIC_VAT: &str = "Imprint — Whenever a nontoken creature dies, you may exile that card. \
If you do, return each other card exiled with this artifact to its owner's graveyard.\n{3}, \
{T}: Create a token that's a copy of a card exiled with this artifact. It gains haste. Exile it \
at the beginning of the next end step.";

/// Cast `murder` at `victim` and resolve it plus the dies trigger the death
/// raises, answering the Vat's optional imprint exile with yes (CR 603.5).
fn murder_and_imprint(runner: &mut GameRunner, murder: ObjectId, victim: ObjectId) {
    runner
        .cast(murder)
        .target_object(victim)
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();
}

/// CR 607.2a: the imprint trigger's exile and the ability that reads "exiled
/// with this artifact" are linked, so the pile holds exactly the cards the
/// trigger exiled — the first imprint's card and, after the second imprint,
/// both cards. CR 608.2c: the trigger follows its instructions in order, so
/// "return each other card exiled with this artifact" excludes the card the
/// same resolution just exiled. The first imprint has no other card, so its
/// card stays exiled; the second imprint returns only the first card to its
/// owner's graveyard while the second card stays exiled.
#[test]
fn mimic_vat_imprint_returns_only_the_previously_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 301.1: Mimic Vat is a printed noncreature artifact, {3}.
    let vat = scenario
        .add_artifact_from_oracle(P0, "Mimic Vat", MIMIC_VAT)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}"))
        .id();
    // Two printed vanilla 2/2s for the Vat to imprint, one death each.
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let first_murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    let second_murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    // {1}{B}{B} twice: six black mana pays for both casts in this main phase.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&vat].zone,
        Zone::Battlefield,
        "reach: Mimic Vat is on the battlefield"
    );

    // First imprint: the pile is empty, so nothing is returned and the card
    // this resolution exiled must stay in exile.
    murder_and_imprint(&mut runner, first_murder, first);
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Exile,
        "the first imprinted card stays exiled: the pile held no other card"
    );

    // Second imprint: `first` is the only other card in the pile, so it returns
    // to its owner's graveyard while `second` — the card just exiled — stays.
    murder_and_imprint(&mut runner, second_murder, second);
    let state = runner.state();
    assert_eq!(
        state.objects[&second].zone,
        Zone::Exile,
        "CR 608.2c: the card this resolution just exiled is not returned"
    );
    assert_eq!(
        state.objects[&first].zone,
        Zone::Graveyard,
        "CR 607.2a: the previously exiled card returns to a graveyard"
    );
    assert_eq!(
        state.objects[&first].owner, P1,
        "the returned card is owned by P1"
    );
    assert!(
        state.players[1].graveyard.contains(&first),
        "CR 607.2a: it returns to its owner's graveyard"
    );
    assert_eq!(
        state.objects[&vat].zone,
        Zone::Battlefield,
        "the Vat itself never left the battlefield"
    );
}

/// Verbatim Oracle text of Mirror of Life Trapping (Scryfall).
const MIRROR_OF_LIFE_TRAPPING: &str = "Whenever a creature enters, if it was cast, exile it, then \
return all other permanent cards exiled with this artifact to the battlefield under their owners' \
control.";

/// CR 607.2a: the trigger's exile and the ability that reads "exiled with this
/// artifact" are linked, so the pile holds exactly the creature cards the
/// artifact's triggers exiled. CR 608.2c: the trigger follows its instructions
/// in order, so the creature cast second is exiled first and is then excluded
/// from "all other permanent cards exiled with this artifact" — only the
/// creature exiled by the earlier trigger comes back. The first cast's trigger
/// has no other card, so its creature stays exiled; the second cast's trigger
/// returns that first creature to the battlefield while the second, just
/// exiled, stays in exile.
#[test]
fn mirror_of_life_trapping_returns_the_earlier_creature_not_the_one_just_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 301.1: Mirror of Life Trapping is a printed noncreature artifact, {4}.
    let mirror = scenario
        .add_artifact_from_oracle(P0, "Mirror of Life Trapping", MIRROR_OF_LIFE_TRAPPING)
        .with_mana_cost(parse_mtgjson_mana_cost("{4}"))
        .id();
    // Two printed vanilla 2/2s for P0 to cast, one per Mirror trigger.
    let first = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Runeclaw Bear", 2, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    // {1}{G} twice: four green mana pays for both casts in this main phase.
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&mirror].zone,
        Zone::Battlefield,
        "reach: Mirror of Life Trapping is on the battlefield"
    );

    // First cast: the pile is empty, so nothing is returned and the creature
    // this resolution exiled must stay in exile.
    runner.cast(first).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Exile,
        "reach: the first cast creature is exiled by the Mirror's trigger"
    );

    // Second cast: `first` is the only other card in the pile, so it returns to
    // the battlefield while `second` — the creature just exiled — stays exiled.
    runner.cast(second).resolve();
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(
        state.objects[&second].zone,
        Zone::Exile,
        "CR 608.2c: the creature this resolution just exiled is not returned"
    );
    assert_eq!(
        state.objects[&first].zone,
        Zone::Battlefield,
        "CR 607.2a: the previously exiled creature returns to the battlefield"
    );
    assert_eq!(
        state.objects[&first].owner, P0,
        "the returned creature is owned by P0"
    );
    assert_eq!(
        state.objects[&mirror].zone,
        Zone::Battlefield,
        "the Mirror itself never left the battlefield"
    );
}

/// Verbatim Oracle text of Moonring Mirror (Scryfall).
const MOONRING_MIRROR: &str = "Whenever you draw a card, exile the top card of your library face down.\nAt the beginning of your upkeep, you may exile all cards from your hand face down. If you do, put all other cards you own exiled with this artifact into your hand.";

/// Drive the game with real actions until `done` holds: pass priority, decline
/// to attack or block, and accept the only optional effect this board offers.
/// Returns how many `OptionalEffectChoice` prompts were answered. Any other
/// prompt fails the test by name.
fn cycle_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) -> usize {
    let mut optional_choices = 0;
    for _ in 0..600 {
        if done(runner) {
            return optional_choices;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            // CR 608.2d: the only optional effect here is Moonring Mirror's
            // "you may exile all cards from your hand"; accept it.
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_choices += 1;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the turn cycle advances");
    }
    panic!("the turn cycle never reached the awaited state");
}

/// CR 607.2a: every card Moonring Mirror's triggers exile is linked to it, so
/// "all other cards you own exiled with this artifact" reads exactly the pile
/// those triggers built. CR 608.2c: the upkeep trigger follows its instructions
/// in order, so the hand it just exiled is not "other" and cannot come back
/// within the same resolution, while the card an earlier upkeep exiled is not
/// excluded and does.
///
/// Board: the Mirror plus one printed vanilla creature in P0's hand. At P0's
/// first upkeep the optional hand exile takes that creature; the pile is empty
/// (P0 has not drawn yet), so nothing returns. At the draw step P0 draws the
/// stacked top card (the Mirror's own "whenever you draw" trigger exiles the
/// next library card, which this test ignores). At P0's second upkeep the
/// optional hand exile takes the drawn card: "all other cards you own exiled
/// with this artifact" returns the first upkeep's creature to hand, and the
/// card just exiled stays in exile.
#[test]
fn moonring_mirror_upkeep_returns_the_earlier_hand_exile_not_the_one_just_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 301.1: Moonring Mirror is a printed noncreature artifact, {5}.
    let mirror = scenario
        .add_artifact_from_oracle(P0, "Moonring Mirror", MOONRING_MIRROR)
        .with_mana_cost(parse_mtgjson_mana_cost("{5}"))
        .id();
    // The only card in hand for the first upkeep's optional exile.
    let first = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    // Both players must draw through the turn cycle without decking. P0's top
    // card is added last, so `drawn` is what its first draw step takes; the
    // card beneath it feeds the Mirror's own draw trigger.
    for n in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {n}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {n}"));
    }
    let drawn = scenario.add_card_to_library_top(P0, "P0 Draw");
    scenario.add_card_to_library_top(P1, "P1 Draw");
    let mut runner = scenario.build();
    let start = runner.state().turn_number;
    assert_eq!(
        runner.state().objects[&mirror].zone,
        Zone::Battlefield,
        "reach: Moonring Mirror is on the battlefield"
    );

    // First upkeep (P0's next turn): the optional hand exile takes `first`, and
    // the pile is empty, so nothing comes back.
    let choices = cycle_until(&mut runner, |runner| {
        runner.state().objects[&first].zone == Zone::Exile
    });
    assert_eq!(choices, 1, "the upkeep trigger offered its optional exile");
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Exile,
        "the first upkeep's exile takes the only card in hand"
    );
    assert_eq!(runner.state().active_player, P0, "reach: P0's upkeep");
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "reach: the upkeep step"
    );
    assert!(
        runner.state().turn_number >= start + 2,
        "reach: the exile happened on P0's next turn"
    );

    // Draw step: P0 draws the stacked `drawn`; the Mirror's draw trigger then
    // exiles the next library card, which this test does not track.
    let choices = cycle_until(&mut runner, |runner| {
        runner.state().players[0].hand.contains(&drawn)
    });
    assert_eq!(choices, 0, "the draw step asks nothing optional");
    assert_eq!(runner.state().active_player, P0, "reach: P0's draw step");
    assert!(
        runner.state().turn_number < start + 4,
        "reach: the draw is before P0's second upkeep"
    );
    assert_eq!(
        runner.state().objects[&drawn].zone,
        Zone::Hand,
        "reach: P0 drew the stacked card"
    );

    // Second upkeep (two turns later): the optional hand exile takes `drawn`,
    // and "all other cards you own exiled with this artifact" returns `first`
    // while the card just exiled is not returned (CR 608.2c).
    let choices = cycle_until(&mut runner, |runner| {
        let state = runner.state();
        state.objects[&first].zone == Zone::Hand && state.objects[&drawn].zone == Zone::Exile
    });
    assert_eq!(choices, 1, "the second upkeep offered its optional exile");
    let state = runner.state();
    assert_eq!(state.active_player, P0, "reach: P0's second upkeep");
    assert_eq!(state.phase, Phase::Upkeep, "reach: the upkeep step");
    assert!(
        state.turn_number >= start + 4,
        "reach: the second upkeep is two turns later"
    );
    assert_eq!(
        state.objects[&first].zone,
        Zone::Hand,
        "CR 607.2a: the earlier upkeep's exiled card returns"
    );
    assert!(
        state.players[0].hand.contains(&first),
        "the earlier card is back in its owner's hand"
    );
    assert_eq!(
        state.objects[&drawn].zone,
        Zone::Exile,
        "CR 608.2c: the card this resolution just exiled is not returned"
    );
    assert_eq!(
        state.objects[&mirror].zone,
        Zone::Battlefield,
        "the Mirror itself never left the battlefield"
    );
}
