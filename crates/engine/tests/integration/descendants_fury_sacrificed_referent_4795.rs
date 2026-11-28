//! Runtime regression for Descendants' Fury (#4795), driven end-to-end through
//! the real `apply` pipeline (combat → optional-sacrifice trigger → RevealUntil).
//!
//! Descendants' Fury (Scryfall-verified Oracle): "Whenever one or more creatures
//! you control deal combat damage to a player, you may sacrifice one of them. If
//! you do, reveal cards from the top of your library until you reveal a creature
//! card that shares a creature type with the sacrificed creature. Put that card
//! onto the battlefield and the rest on the bottom of your library in a random
//! order."
//!
//! Three class-level defects were fixed:
//!
//!   1. The trigger's `Effect::Sacrifice { target: TrackedSet(0) }` derives its
//!      eligible pool ("one of them") from the `CombatDamageDealtToPlayer` event's
//!      source set. The sacrifice resolver matched the raw `TrackedSet(0)`
//!      sentinel directly against `matches_target_filter`, whose id-level ladder
//!      never reaches the combat-damage rung — so the pool was empty and the
//!      accepted sacrifice was a silent no-op. Fixed by routing the target filter
//!      through the single-authority `resolve_tracked_set_sentinel` (CR 510.2 +
//!      CR 608.2c), the same binder every other tracked-set consumer uses.
//!
//!   2. "the sacrificed creature" parses to `SharesQuality { reference:
//!      CostPaidObject }`. The sacrifice is an EFFECT (not a cost), so the chain
//!      resolver captures the sacrificed creature into `effect_context_object`,
//!      never `cost_paid_object`. The `TargetFilter::CostPaidObject` runtime arms
//!      (`filter.rs` + `targeting.rs`) read only `cost_paid_object`, so the
//!      reference resolved to nothing and the reveal dug past the shared-type
//!      card. Fixed by implementing the documented `cost_paid_object →
//!      effect_context_object` fallback ladder (CR 608.2k) that
//!      `ObjectScope::CostPaidObject`'s P/T and mana-value arms already use.
//!
//!   3. The up-front "you may" feasibility gate
//!      (`sacrifice::optional_sacrifice_is_infeasible`) consulted the candidate
//!      pool only for class-shaped filters. This trigger's "one of them" binds
//!      to `TargetFilter::SpecificObject` (or an `Or` of them, one per damage
//!      source), which that predicate rejected, so the gate failed open and the
//!      prompt opened even when the bound object could not legally be sacrificed
//!      (Quark, Humble Proprietor bars Treasure tokens). Treating
//!      `SpecificObject` as pool-enumerable makes the gate judge the bound
//!      object through `sacrifice_candidates`, which applies the chooser's
//!      control and `can_sacrifice_for_ability` (CR 608.2d + CR 510.2).
//!
//! REVERT-PROOF: reverting any fix flips a positive assertion below —
//!   * revert #1: the attacker stays on the battlefield (never sacrificed) and
//!     the Goblin never enters — `sacrificed to Graveyard` / `Goblin on
//!     Battlefield` both fail;
//!   * revert #2: the attacker IS sacrificed but the reveal digs the whole
//!     library and bottoms it — `Goblin on Battlefield` fails while the reach
//!     guard (attacker sacrificed) still holds;
//!   * revert #3: the Quark-barred lone damage source is offered the "you may
//!     sacrifice" prompt — `..._barred_lone_damage_source_gets_no_offer` fails
//!     while the second-attacker control keeps passing.

use super::rules::{run_combat, GameScenario, Phase, P0, P1};
use crate::support::shared_card_db;
use engine::game::game_object::GameObject;
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const DESCENDANTS_FURY: &str = "Whenever one or more creatures you control deal combat damage to a player, you may sacrifice one of them. If you do, reveal cards from the top of your library until you reveal a creature card that shares a creature type with the sacrificed creature. Put that card onto the battlefield and the rest on the bottom of your library in a random order.";

/// Push both `card_types` and `base_card_types` so the subtype survives a layer
/// recompute (which reverts `card_types` from `base_card_types`).
fn make_creature(obj: &mut GameObject, subtype: &str) {
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push(subtype.into());
    obj.base_card_types.core_types.push(CoreType::Creature);
    obj.base_card_types.subtypes.push(subtype.into());
}

fn make_noncreature(obj: &mut GameObject, core: CoreType) {
    obj.card_types.core_types.push(core);
    obj.base_card_types.core_types.push(core);
}

/// Accept the "you may sacrifice one of them" prompt and drive the resolution to
/// a settled priority, submitting any interactive sacrifice/reveal choice via the
/// supplied `sacrifice_pick` selector (chooses which eligible creature to sac when
/// an `EffectZoneChoice` surfaces — single-attacker cases fast-path with none).
fn resolve_trigger(
    runner: &mut super::rules::GameRunner,
    mut sacrifice_pick: impl FnMut(&[ObjectId]) -> Vec<ObjectId>,
) {
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional sacrifice");
            }
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                let pick = sacrifice_pick(&cards);
                assert!(!pick.is_empty() && pick.len() <= count.max(1));
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("submit sacrifice selection");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected WaitingFor while resolving Descendants' Fury: {other:?}"),
        }
    }
}

/// Primary end-to-end: one Goblin attacker deals combat damage, is sacrificed,
/// and the reveal digs past a non-creature card to put the library Goblin (shares
/// a creature type with the sacrificed Goblin) onto the battlefield.
#[test]
fn descendants_fury_reveals_and_battlefields_shared_type_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();

    // Library cards (order fixed below): miss (non-creature), goblin_match
    // (shares type), non_match (creature, different type).
    let miss = scenario.add_card_to_library_top(P0, "Non Creature Miss");
    let goblin_match = scenario.add_card_to_library_top(P0, "Library Goblin");
    let non_match = scenario.add_card_to_library_top(P0, "Library Bear");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Bear".into()];

    // Library top → bottom: miss, goblin_match, non_match.
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != miss && id != goblin_match && id != non_match);
        lib.insert(0, non_match);
        lib.insert(0, goblin_match);
        lib.insert(0, miss);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&goblin_match).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&non_match).unwrap(), "Bear");
        make_noncreature(s.objects.get_mut(&miss).unwrap(), CoreType::Sorcery);
    }

    let p1_before = runner.life(P1);
    run_combat(&mut runner, vec![attacker], vec![]);
    resolve_trigger(&mut runner, |_| unreachable!("single attacker fast-paths"));

    let s = runner.state();
    // Reach guard: combat actually happened.
    assert_eq!(
        runner.life(P1),
        p1_before - 2,
        "attacker must deal 2 combat damage to P1"
    );
    // Reach guard (fix #1): the attacker was sacrificed — proves the reveal was
    // gated on a real sacrifice, not a no-op accept.
    assert_eq!(
        s.objects[&attacker].zone,
        Zone::Graveyard,
        "the accepted 'sacrifice one of them' must actually sacrifice the attacker (fix #1)"
    );
    // Primary assertion (fix #2): the shared-type Goblin entered the battlefield,
    // and the reveal DUG PAST the non-creature miss to find it.
    assert_eq!(
        s.objects[&goblin_match].zone,
        Zone::Battlefield,
        "the revealed creature sharing a type with the sacrificed Goblin must enter the battlefield (fix #2)"
    );
    // The reveal stopped at the FIRST match — the later Bear must NOT enter and
    // the non-creature miss returns to the (bottom of the) library.
    assert_eq!(
        s.objects[&non_match].zone,
        Zone::Library,
        "the reveal stops at the first shared-type match; the later Bear stays in the library"
    );
    assert_eq!(
        s.objects[&miss].zone,
        Zone::Library,
        "the revealed non-creature miss goes to the bottom of the library"
    );
    // The rest pile (miss) is on the bottom, below the never-revealed remainder
    // (non_match). Both are in the library; miss is at or below non_match.
    let lib: Vec<ObjectId> = s.players[0].library.iter().copied().collect();
    assert!(
        lib.contains(&miss) && lib.contains(&non_match),
        "rest cards remain in the library"
    );
}

/// Negative: no creature in the library shares a type with the sacrificed
/// creature. Per CR 701.20a the reveal exhausts the library, nothing enters, and
/// every revealed card returns to the bottom. Discriminates fix #2: if the
/// `CostPaidObject` reference wrongly matched (or matched nothing and any
/// creature satisfied a vacuous filter) a card could still enter.
#[test]
fn descendants_fury_no_shared_type_reveals_whole_library_nothing_enters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();

    // Library: a Bear creature (different type) and a non-creature — no shared
    // type with the sacrificed Goblin.
    let bear = scenario.add_card_to_library_top(P0, "Library Bear");
    let sorcery = scenario.add_card_to_library_top(P0, "Library Sorcery");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Bear".into()];
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != bear && id != sorcery);
        lib.insert(0, bear);
        lib.insert(0, sorcery);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&bear).unwrap(), "Bear");
        make_noncreature(s.objects.get_mut(&sorcery).unwrap(), CoreType::Sorcery);
    }

    run_combat(&mut runner, vec![attacker], vec![]);
    resolve_trigger(&mut runner, |_| unreachable!("single attacker fast-paths"));

    let s = runner.state();
    // Reach guard: the sacrifice still happens.
    assert_eq!(
        s.objects[&attacker].zone,
        Zone::Graveyard,
        "the attacker is sacrificed even when no library creature shares its type"
    );
    // Nothing enters the battlefield; both revealed cards stay in the library.
    assert_eq!(
        s.objects[&bear].zone,
        Zone::Library,
        "no shared type → Bear does not enter"
    );
    assert_eq!(
        s.objects[&sorcery].zone,
        Zone::Library,
        "non-creature never enters"
    );
    assert!(
        !s.objects.values().any(|o| o.zone == Zone::Battlefield
            && o.owner == P0
            && (o.id == bear || o.id == sorcery)),
        "no revealed card was put onto the battlefield"
    );
}

/// Multi-eligible interactive path: two attackers of DIFFERENT creature types
/// deal combat damage. The "sacrifice one of them" surfaces an `EffectZoneChoice`
/// over both; the CHOSEN creature's type drives the reveal. Choosing the Goblin
/// must battlefield the library Goblin (not the Elf), proving the reveal reads the
/// SELECTED sacrifice's type via `effect_context_object`.
#[test]
fn descendants_fury_multi_attacker_chosen_type_drives_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Descendants' Fury", 0, 3, DESCENDANTS_FURY);
    let goblin_attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();
    let elf_attacker = scenario.add_creature(P0, "Elf Attacker", 2, 2).id();

    // Library top: an Elf (would match if the Elf were sacrificed), then a Goblin
    // (matches the chosen Goblin). Choosing the Goblin must skip the top Elf and
    // battlefield the Goblin.
    let lib_goblin = scenario.add_card_to_library_top(P0, "Library Goblin");
    let lib_elf = scenario.add_card_to_library_top(P0, "Library Elf");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];
    {
        let lib = &mut runner.state_mut().players[0].library;
        lib.retain(|&id| id != lib_goblin && id != lib_elf);
        lib.insert(0, lib_goblin);
        lib.insert(0, lib_elf);
    }
    {
        let s = runner.state_mut();
        make_creature(s.objects.get_mut(&goblin_attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&elf_attacker).unwrap(), "Elf");
        make_creature(s.objects.get_mut(&lib_goblin).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&lib_elf).unwrap(), "Elf");
    }

    run_combat(&mut runner, vec![goblin_attacker, elf_attacker], vec![]);

    // The interactive path must surface an EffectZoneChoice over BOTH attackers;
    // choose the Goblin.
    let mut saw_choice = false;
    resolve_trigger(&mut runner, |eligible| {
        saw_choice = true;
        assert!(
            eligible.contains(&goblin_attacker) && eligible.contains(&elf_attacker),
            "both combat-damaging attackers are eligible to sacrifice; got {eligible:?}"
        );
        vec![goblin_attacker]
    });
    assert!(
        saw_choice,
        "two eligible attackers must surface an interactive sacrifice choice"
    );

    let s = runner.state();
    assert_eq!(
        s.objects[&goblin_attacker].zone,
        Zone::Graveyard,
        "the chosen Goblin is sacrificed"
    );
    assert_eq!(
        s.objects[&elf_attacker].zone,
        Zone::Battlefield,
        "the unchosen Elf attacker is not sacrificed"
    );
    // The CHOSEN Goblin's type drives the reveal: the library Goblin enters, the
    // top Library Elf is skipped (dug past → returns to the library).
    assert_eq!(
        s.objects[&lib_goblin].zone,
        Zone::Battlefield,
        "the reveal reads the CHOSEN sacrifice's (Goblin) type and battlefields the library Goblin"
    );
    assert_eq!(
        s.objects[&lib_elf].zone,
        Zone::Library,
        "the top Library Elf does not share the chosen Goblin's type — dug past, returned to library"
    );
}

/// Pass priority while the resolving trigger drains, returning the first
/// interactive prompt the engine parks on (`None` once the stack is empty at a
/// priority window). Answers no prompt, so a caller can assert whether an offer
/// opened at all.
fn drive_until_sacrifice_prompt(runner: &mut super::rules::GameRunner) -> Option<WaitingFor> {
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return None,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
            other => return Some(other),
        }
    }
    panic!("Descendants' Fury's trigger never settled");
}

/// Quark, Humble Proprietor — "You can't sacrifice Treasure tokens." — plus
/// Descendants' Fury, with Quark's controller's lone Treasure *token* also being
/// an artifact creature so it is the only creature that deals combat damage.
/// The animation source is deliberately not load-bearing: this test pins the
/// sacrifice gate's `SpecificObject` handling, so the scenario builder supplies
/// the creature type directly rather than routing through an Aura whose
/// animation would depend on the cast/attach pipeline.
///
/// CR 510.2 binds "one of them" to that one object; CR 608.2d + CR 701.21a make
/// the sacrifice an impossible option because its chooser is barred from taking
/// it. The "you may" prompt must not open, nothing is sacrificed, and the gated
/// reveal never runs.
#[test]
fn descendants_fury_quark_barred_lone_damage_source_gets_no_offer() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let fury = scenario.add_real_card(P0, "Descendants' Fury", Zone::Battlefield, db);
    let treasure = scenario
        .add_creature_from_oracle(
            P0,
            "Treasure",
            2,
            2,
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .with_subtypes(vec!["Treasure"])
        .as_artifact_creature()
        .id();

    let mut runner = scenario.build();
    // CR 111.10b: Quark's prohibition names Treasure TOKENS.
    runner
        .state_mut()
        .objects
        .get_mut(&treasure)
        .unwrap()
        .is_token = true;

    let p1_before = runner.life(P1);
    run_combat(&mut runner, vec![treasure], vec![]);

    // Reach guards: the Treasure attacked as an artifact creature, dealt damage,
    // and the printed trigger is on the stack for the combat-damage event.
    assert!(
        runner.state().objects[&treasure]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the fixture Treasure must be an artifact creature"
    );
    assert!(
        runner.life(P1) < p1_before,
        "the Treasure must deal combat damage to P1"
    );
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == fury
        )),
        "Descendants' Fury's combat-damage trigger must be on the stack: {:?}",
        runner.state().stack
    );

    assert!(
        drive_until_sacrifice_prompt(&mut runner).is_none(),
        "the Quark-barred Treasure is the only bound candidate, so no \
         optional-sacrifice prompt may open; got {:?}",
        runner.state().waiting_for
    );

    let s = runner.state();
    assert_eq!(
        s.objects[&treasure].zone,
        Zone::Battlefield,
        "the barred Treasure must survive — nothing was sacrificed"
    );
    assert!(
        s.players[0].graveyard.is_empty(),
        "no sacrifice (and therefore no sacrifice payoff) may happen"
    );
    assert!(s.stack.is_empty());
}

/// Control for the test above: a second legal attacker also deals combat
/// damage, so "one of them" binds an `Or` whose Goblin leg is a legal
/// candidate. The prompt must still open; accepting sacrifices only the Goblin
/// (the Quark-barred Treasure is never offered) and the `If you do` reveal
/// battlefields the library Goblin that shares its creature type.
#[test]
fn descendants_fury_quark_barred_treasure_with_legal_second_attacker_still_offers() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let fury = scenario.add_real_card(P0, "Descendants' Fury", Zone::Battlefield, db);
    let treasure = scenario
        .add_creature_from_oracle(
            P0,
            "Treasure",
            2,
            2,
            "{T}, Sacrifice this artifact: Add one mana of any color.",
        )
        .with_subtypes(vec!["Treasure"])
        .as_artifact_creature()
        .id();
    let goblin_attacker = scenario.add_creature(P0, "Goblin Attacker", 2, 2).id();
    let lib_goblin = scenario.add_card_to_library_top(P0, "Library Goblin");

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into()];
    {
        let s = runner.state_mut();
        s.objects.get_mut(&treasure).unwrap().is_token = true;
        make_creature(s.objects.get_mut(&goblin_attacker).unwrap(), "Goblin");
        make_creature(s.objects.get_mut(&lib_goblin).unwrap(), "Goblin");
    }

    run_combat(&mut runner, vec![treasure, goblin_attacker], vec![]);
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == fury
        )),
        "Descendants' Fury's combat-damage trigger must be on the stack: {:?}",
        runner.state().stack
    );

    let prompt = drive_until_sacrifice_prompt(&mut runner);
    assert!(
        matches!(prompt, Some(WaitingFor::OptionalEffectChoice { player, .. }) if player == P0),
        "a legal second candidate keeps the sacrifice offer performable, got {prompt:?}"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the sacrifice");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "the Quark-barred Treasure is not a candidate, so there is nothing to choose between"
    );
    runner.advance_until_stack_empty();

    let s = runner.state();
    assert_eq!(
        s.objects[&goblin_attacker].zone,
        Zone::Graveyard,
        "the legal Goblin is the one sacrificed"
    );
    assert_eq!(
        s.objects[&treasure].zone,
        Zone::Battlefield,
        "the Quark-barred Treasure is kept"
    );
    assert_eq!(
        s.objects[&lib_goblin].zone,
        Zone::Battlefield,
        "the accepted sacrifice runs the If-you-do reveal, which battlefields the shared-type Goblin"
    );
    assert!(s.stack.is_empty());
}
