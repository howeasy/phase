//! CR 106.6 spell-property spend restrictions — production-path witnesses.
//!
//! Six printed cards whose restricted mana is gated on a static property of the
//! spell being cast, each driven through the real pipeline: the source's printed
//! mana ability is activated, the produced unit is observed in the pool carrying
//! its parsed restriction, and the printed spell is then cast through
//! `GameAction::CastSpell` / the fluent cast driver with the restriction live.
//!
//! - Jeweled Lotus / Queen's Diamond — "your commander" (CR 903.3d + CR 108.3)
//! - Thieving Varmint — "spells you don't own" (CR 108.3), reached through
//!   Nita, Forum Conciliator's exile-and-cast grant
//! - Jasmine Boreal of the Seven — "creature spells with no abilities"
//!   (CR 113.1 + CR 113.3)
//! - Titans' Nest — "one or more colors without {X} in its mana cost"
//!   (CR 105.2 + CR 202.1 + CR 107.3)
//! - Corrupted Crossroads — "a spell with devoid", the existing keyword leaf,
//!   exercised here because this change extends its parser `alt`
//!
//! Every negative row pairs `can_cast_object_now` (the engine's own offer
//! predicate) against the same spell with plain mana as the reach guard, and
//! then casts the control so the pool proves the restricted units were withheld
//! in a real payment, not merely that an action was refused. The `{X}` row
//! cannot use the offer predicate — at X=0 Fireball costs {R} and a Mountain
//! pays it — so it announces X=1 and asserts the real payment is refused.
//!
//! These tests read the committed fixture through `add_real_card`, so the card
//! data must be regenerated (`./scripts/gen-card-data.sh` +
//! `python3 scripts/gen-test-fixture.py`) for the parsed restrictions to exist;
//! the per-source reach guards below fail loudly if they do not.
//!
//! Revert-proof: `SpellProperty::matches` returning `true`, or `build_spell_meta`
//! dropping the commander conjunct / the no-abilities census, turns the hostile
//! pool assertions red because the restricted units are then consumed by the
//! ineligible spell.

use engine::database::card_db::CardDatabase;
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaRestriction, ManaType, ManaUnit, SpellProperty};
use engine::types::phase::Phase;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};

use crate::support::shared_card_db;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Activate a mana ability that asks for a color and answer the prompt.
fn activate_and_choose(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    color: ManaType,
) {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the mana ability must be activatable");
    assert!(
        matches!(result.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "{source:?} must ask for a color, got {:?}",
        result.waiting_for
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(color),
            count: 1,
        })
        .expect("the color choice must resolve");
}

/// Activate a fixed-output mana ability and return to priority.
fn activate_fixed(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the mana ability must be activatable");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "{source:?} must resolve without a prompt, got {:?}",
        result.waiting_for
    );
}

fn floating_units(runner: &GameRunner) -> Vec<ManaUnit> {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .units()
        .collect()
}

/// Reach guard: every floating unit carries each expected parsed restriction.
fn assert_units_restricted(runner: &GameRunner, expected: &[ManaRestriction], count: usize) {
    let units = floating_units(runner);
    assert_eq!(units.len(), count, "floating unit count: {units:?}");
    for unit in &units {
        for restriction in expected {
            assert!(
                unit.restrictions.contains(restriction),
                "floating {:?} unit is missing {restriction:?}; restrictions={:?}",
                unit.color,
                unit.restrictions
            );
        }
    }
}

/// The sources named by the spell's payment record.
fn spent_sources(runner: &GameRunner, spell: ObjectId) -> Vec<ObjectId> {
    runner.state().objects[&spell]
        .mana_spent_source_snapshots
        .iter()
        .map(|snapshot| snapshot.source_id)
        .collect()
}

fn commander_property() -> ManaRestriction {
    ManaRestriction::OnlyForSpellProperty(SpellProperty::Commander)
}

fn not_owned_property() -> ManaRestriction {
    ManaRestriction::OnlyForSpellProperty(SpellProperty::NotOwnedByCaster)
}

fn no_abilities_property() -> ManaRestriction {
    ManaRestriction::OnlyForSpellProperty(SpellProperty::NoAbilities)
}

fn no_x_property() -> ManaRestriction {
    ManaRestriction::OnlyForSpellProperty(SpellProperty::WithoutXInManaCost)
}

// ---------------------------------------------------------------------------
// Row 7: Jeweled Lotus — "your commander".
// ---------------------------------------------------------------------------

#[test]
fn jeweled_lotus_commander_gate_pays_the_commanders_cost() {
    let mut scenario = main_phase();
    let lotus = scenario.add_real_card(P0, "Jeweled Lotus", Zone::Battlefield, db());
    let commander = scenario.add_real_card(P0, "Azusa, Lost but Seeking", Zone::Hand, db());
    scenario.with_commander(commander);
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    activate_and_choose(&mut runner, lotus, 0, ManaType::Green);
    assert_units_restricted(&runner, &[commander_property()], 3);
    assert!(
        can_cast_object_now(runner.state(), P0, commander),
        "the owned commander must be offered while the Lotus mana floats"
    );

    runner.cast(commander).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "all three commander-restricted units paid for Azusa"
    );
    assert!(
        spent_sources(&runner, commander).contains(&lotus),
        "the commander's payment must name Jeweled Lotus: {:?}",
        spent_sources(&runner, commander)
    );
}

#[test]
fn jeweled_lotus_commander_gate_rejects_non_commander_and_foreign_commander() {
    // Hostile 1: a plain creature is not a commander.
    let mut scenario = main_phase();
    let lotus = scenario.add_real_card(P0, "Jeweled Lotus", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, lotus, 0, ManaType::Green);
    assert_units_restricted(&runner, &[commander_property()], 3);
    assert!(
        !can_cast_object_now(runner.state(), P0, bears),
        "Grizzly Bears must not consume commander-restricted mana"
    );

    // Control: the same spell is castable with plain mana, and the restricted
    // units stay in the pool through a real payment.
    let mut scenario = main_phase();
    let lotus = scenario.add_real_card(P0, "Jeweled Lotus", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, lotus, 0, ManaType::Green);
    assert!(can_cast_object_now(runner.state(), P0, bears));
    runner.cast(bears).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        3,
        "the control cast must leave every restricted unit unspent"
    );
    assert!(
        !spent_sources(&runner, bears).contains(&lotus),
        "the control cast must not name Jeweled Lotus"
    );

    // Hostile 2: an opponent-owned commander. CR 903.3d + CR 108.3: "your
    // commander" requires the caster to own it. The PlayFromExile grant is the
    // scenario staging that makes an opponent-owned commander castable at all;
    // the spend restriction is what this row measures.
    let foreign_commander_board = |with_control_mana: bool| {
        let mut scenario = main_phase();
        let lotus = scenario.add_real_card(P0, "Jeweled Lotus", Zone::Battlefield, db());
        let foreign = scenario.add_real_card(P1, "Azusa, Lost but Seeking", Zone::Exile, db());
        if with_control_mana {
            // Azusa costs {2}{G}: two Forests pay only two green, so the reach
            // guard needs a third basic or it fails on mana, not on the Lotus
            // restriction this row measures.
            scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
            scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
            scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
        }
        let mut runner = scenario.build();
        let object = runner
            .state_mut()
            .objects
            .get_mut(&foreign)
            .expect("the exiled foreign commander exists");
        object.is_commander = true;
        object
            .casting_permissions
            .push(CastingPermission::PlayFromExile {
                provenance: PlayFromExileProvenance::Impulse,
                duration: Duration::Permanent,
                granted_to: P0,
                mode: CardPlayMode::Cast,
                frequency: CastFrequency::Unlimited,
                source_id: None,
                invalidation: None,
                exiled_by_ability_controller: Some(P0),
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_modifier: None,
                alt_ability_cost: None,
                without_paying_mana_cost: false,
                land_enter_tapped: EtbTapState::Unspecified,
            });
        activate_and_choose(&mut runner, lotus, 0, ManaType::Green);
        (runner, lotus, foreign)
    };

    let (runner, _lotus, foreign) = foreign_commander_board(false);
    assert_units_restricted(&runner, &[commander_property()], 3);
    assert!(
        !can_cast_object_now(runner.state(), P0, foreign),
        "an opponent-owned commander is not \"your commander\""
    );

    let (mut runner, lotus, foreign) = foreign_commander_board(true);
    assert!(
        can_cast_object_now(runner.state(), P0, foreign),
        "reach guard: the foreign commander is castable with the control Forests"
    );
    runner.cast(foreign).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        3,
        "the foreign commander's control cast must leave the Lotus mana unspent"
    );
    assert!(
        !spent_sources(&runner, foreign).contains(&lotus),
        "the foreign commander must not consume commander-restricted mana"
    );
}

// ---------------------------------------------------------------------------
// Row 8: Queen's Diamond — "your commander".
// ---------------------------------------------------------------------------

#[test]
fn queens_diamond_commander_gate_pays_the_commanders_cost() {
    let mut scenario = main_phase();
    let diamond = scenario.add_real_card(P0, "Queen's Diamond", Zone::Battlefield, db());
    scenario.with_counter(diamond, CounterType::Generic("charge".to_string()), 3);
    let commander = scenario.add_real_card(P0, "Azusa, Lost but Seeking", Zone::Hand, db());
    scenario.with_commander(commander);
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    activate_and_choose(&mut runner, diamond, 0, ManaType::Green);
    assert_units_restricted(&runner, &[commander_property()], 3);
    assert!(can_cast_object_now(runner.state(), P0, commander));

    runner.cast(commander).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "all three commander-restricted units paid for Azusa"
    );
    assert!(
        spent_sources(&runner, commander).contains(&diamond),
        "the commander's payment must name Queen's Diamond"
    );
}

#[test]
fn queens_diamond_commander_gate_rejects_non_commander() {
    let mut scenario = main_phase();
    let diamond = scenario.add_real_card(P0, "Queen's Diamond", Zone::Battlefield, db());
    scenario.with_counter(diamond, CounterType::Generic("charge".to_string()), 3);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, diamond, 0, ManaType::Green);
    assert_units_restricted(&runner, &[commander_property()], 3);
    assert!(
        !can_cast_object_now(runner.state(), P0, bears),
        "Grizzly Bears must not consume commander-restricted mana"
    );

    let mut scenario = main_phase();
    let diamond = scenario.add_real_card(P0, "Queen's Diamond", Zone::Battlefield, db());
    scenario.with_counter(diamond, CounterType::Generic("charge".to_string()), 3);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, diamond, 0, ManaType::Green);
    assert!(can_cast_object_now(runner.state(), P0, bears));
    runner.cast(bears).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        3,
        "the control cast must leave every restricted unit unspent"
    );
    assert!(!spent_sources(&runner, bears).contains(&diamond));
}

// ---------------------------------------------------------------------------
// Row 9: Corrupted Crossroads — "a spell with devoid".
// ---------------------------------------------------------------------------

fn devoid_restriction() -> ManaRestriction {
    ManaRestriction::OnlyForSpellWithKeywordKind(KeywordKind::Devoid)
}

#[test]
fn corrupted_crossroads_devoid_gate_pays_touch_of_the_void() {
    let mut scenario = main_phase();
    let crossroads = scenario.add_real_card(P0, "Corrupted Crossroads", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Wastes", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Wastes", Zone::Battlefield, db());
    let touch = scenario.add_real_card(P0, "Touch of the Void", Zone::Hand, db());
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    activate_and_choose(&mut runner, crossroads, 1, ManaType::Red);
    assert_eq!(
        runner.state().players[0].life,
        life_before - 1,
        "the {{T}}, Pay 1 life ability must charge its life cost"
    );
    assert_units_restricted(&runner, &[devoid_restriction()], 1);
    assert!(
        can_cast_object_now(runner.state(), P0, touch),
        "Touch of the Void is a devoid spell"
    );

    runner.cast(touch).target_player(P1).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the Wastes paid the generic and the Crossroads unit paid {{R}}"
    );
    assert!(
        spent_sources(&runner, touch).contains(&crossroads),
        "Touch of the Void's payment must name Corrupted Crossroads"
    );
}

#[test]
fn corrupted_crossroads_devoid_gate_rejects_non_devoid_spells() {
    let mut scenario = main_phase();
    let crossroads = scenario.add_real_card(P0, "Corrupted Crossroads", Zone::Battlefield, db());
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, crossroads, 1, ManaType::Red);
    assert_units_restricted(&runner, &[devoid_restriction()], 1);
    assert!(
        !can_cast_object_now(runner.state(), P0, bolt),
        "a red spell without devoid must not consume Crossroads mana"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, sol_ring),
        "a colorless spell without devoid must not consume Crossroads mana"
    );

    // Control 1: Bolt with a Mountain; the Crossroads unit stays.
    let mut scenario = main_phase();
    let crossroads = scenario.add_real_card(P0, "Corrupted Crossroads", Zone::Battlefield, db());
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, crossroads, 1, ManaType::Red);
    assert!(can_cast_object_now(runner.state(), P0, bolt));
    runner.cast(bolt).target_player(P1).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "the Crossroads unit must remain unspent"
    );
    assert!(
        !spent_sources(&runner, bolt).contains(&crossroads),
        "Bolt's payment must name the Mountain, not the Crossroads"
    );

    // Control 2: Sol Ring with a Wastes; the Crossroads unit stays.
    let mut scenario = main_phase();
    let crossroads = scenario.add_real_card(P0, "Corrupted Crossroads", Zone::Battlefield, db());
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    scenario.add_real_card(P0, "Wastes", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, crossroads, 1, ManaType::Red);
    assert!(can_cast_object_now(runner.state(), P0, sol_ring));
    runner.cast(sol_ring).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "the Crossroads unit must remain unspent"
    );
    assert!(!spent_sources(&runner, sol_ring).contains(&crossroads));
}

// ---------------------------------------------------------------------------
// Row 10: Thieving Varmint — "spells you don't own", through Nita.
// ---------------------------------------------------------------------------

/// Nita, Forum Conciliator: `{2}, Sacrifice another creature: Exile target
/// instant or sorcery card from an opponent's graveyard. You may cast it this
/// turn, and mana of any type can be spent to cast that spell.` Seed `{2}` and
/// a fodder creature, exile the opponent's Bolt, and return the exiled id.
fn stage_nita_grant(runner: &mut GameRunner) -> ObjectId {
    let nita = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Nita, Forum Conciliator")
        .map(|object| object.id)
        .expect("Nita is on the battlefield");
    let fodder = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Grizzly Bears" && object.zone == Zone::Battlefield)
        .map(|object| object.id)
        .expect("the sacrifice fodder is on the battlefield");
    let bolt = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Lightning Bolt" && object.owner == P1)
        .map(|object| object.id)
        .expect("the opponent's Bolt is in their graveyard");

    runner
        .activate(nita, 0)
        .target_object(bolt)
        .pay_with(&[fodder])
        .resolve();

    let exiled = &runner.state().objects[&bolt];
    assert_eq!(
        exiled.zone,
        Zone::Exile,
        "Nita must exile the opponent's Bolt"
    );
    assert!(
        exiled.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0
        )),
        "Nita must grant P0 the right to cast the exiled Bolt"
    );
    bolt
}

#[test]
fn thieving_varmint_not_owned_spell_consumes_its_mana() {
    let mut scenario = main_phase();
    scenario.add_real_card(P0, "Nita, Forum Conciliator", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    scenario.add_real_card(P1, "Lightning Bolt", Zone::Graveyard, db());
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let varmint = scenario.add_real_card(P0, "Thieving Varmint", Zone::Battlefield, db());
    let mut runner = scenario.build();

    let foreign_bolt = stage_nita_grant(&mut runner);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "Nita's {{2}} must be paid before the Varmint mana floats"
    );

    activate_and_choose(&mut runner, varmint, 0, ManaType::Red);
    assert_units_restricted(&runner, &[not_owned_property()], 2);
    assert!(
        can_cast_object_now(runner.state(), P0, foreign_bolt),
        "the opponent-owned Bolt is a spell P0 does not own"
    );

    runner.cast(foreign_bolt).target_player(P1).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "one Varmint unit paid {{R}}; the second unit must remain"
    );
    assert!(
        spent_sources(&runner, foreign_bolt).contains(&varmint),
        "the opponent-owned Bolt's payment must name Thieving Varmint"
    );
    assert_units_restricted(&runner, &[not_owned_property()], 1);
}

#[test]
fn thieving_varmint_rejects_owned_spells() {
    // Hostile: an own Bolt is not a spell P0 doesn't own.
    let mut scenario = main_phase();
    let varmint = scenario.add_real_card(P0, "Thieving Varmint", Zone::Battlefield, db());
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, varmint, 0, ManaType::Red);
    assert_units_restricted(&runner, &[not_owned_property()], 2);
    assert!(
        !can_cast_object_now(runner.state(), P0, bolt),
        "an owned spell must not consume \"spells you don't own\" mana"
    );

    // Control: a Mountain casts the owned Bolt and both Varmint units stay.
    let mut scenario = main_phase();
    let varmint = scenario.add_real_card(P0, "Thieving Varmint", Zone::Battlefield, db());
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_and_choose(&mut runner, varmint, 0, ManaType::Red);
    assert!(can_cast_object_now(runner.state(), P0, bolt));
    runner.cast(bolt).target_player(P1).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        2,
        "both Varmint units must remain unspent"
    );
    assert!(
        !spent_sources(&runner, bolt).contains(&varmint),
        "the control Bolt must be paid by the Mountain"
    );
    assert_units_restricted(&runner, &[not_owned_property()], 2);
}

// ---------------------------------------------------------------------------
// Row 11: Jasmine Boreal of the Seven — "creature spells with no abilities".
// ---------------------------------------------------------------------------

fn jasmine_restrictions() -> Vec<ManaRestriction> {
    vec![
        ManaRestriction::OnlyForSpellType("Creature".to_string()),
        no_abilities_property(),
    ]
}

#[test]
fn jasmine_boreal_no_abilities_gate_pays_a_vanilla_creature() {
    let mut scenario = main_phase();
    let jasmine =
        scenario.add_real_card(P0, "Jasmine Boreal of the Seven", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let mut runner = scenario.build();

    activate_fixed(&mut runner, jasmine, 0);
    assert_units_restricted(&runner, &jasmine_restrictions(), 2);
    assert!(
        can_cast_object_now(runner.state(), P0, bears),
        "Grizzly Bears is a creature spell with no abilities"
    );

    runner.cast(bears).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{G}} and {{W}} both paid for Grizzly Bears"
    );
    assert!(
        spent_sources(&runner, bears).contains(&jasmine),
        "Grizzly Bears' payment must name Jasmine Boreal"
    );
}

#[test]
fn jasmine_boreal_no_abilities_gate_rejects_a_creature_with_an_ability() {
    let mut scenario = main_phase();
    let jasmine =
        scenario.add_real_card(P0, "Jasmine Boreal of the Seven", Zone::Battlefield, db());
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_fixed(&mut runner, jasmine, 0);
    assert_units_restricted(&runner, &jasmine_restrictions(), 2);
    assert!(
        !can_cast_object_now(runner.state(), P0, elves),
        "Llanowar Elves has an activated ability"
    );

    let mut scenario = main_phase();
    let jasmine =
        scenario.add_real_card(P0, "Jasmine Boreal of the Seven", Zone::Battlefield, db());
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Hand, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_fixed(&mut runner, jasmine, 0);
    assert!(can_cast_object_now(runner.state(), P0, elves));
    runner.cast(elves).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        2,
        "both Jasmine units must remain unspent"
    );
    assert!(!spent_sources(&runner, elves).contains(&jasmine));
}

// ---------------------------------------------------------------------------
// Row 12: Titans' Nest — "one or more colors without {X} in its mana cost".
// ---------------------------------------------------------------------------

fn titans_nest_restrictions() -> Vec<ManaRestriction> {
    vec![
        ManaRestriction::OnlyForSpellWithColorCount {
            comparator: engine::types::ability::Comparator::GE,
            count: 1,
        },
        no_x_property(),
    ]
}

/// Activate Titans' Nest by exiling a graveyard card.
fn activate_titans_nest(runner: &mut GameRunner, nest: ObjectId, graveyard_card: ObjectId) {
    runner
        .activate(nest, 0)
        .pay_with(&[graveyard_card])
        .resolve();
    assert_eq!(
        runner.state().objects[&graveyard_card].zone,
        Zone::Exile,
        "Titans' Nest's cost must exile the chosen graveyard card"
    );
}

#[test]
fn titans_nest_colors_without_x_gate_pays_a_colored_spell() {
    let mut scenario = main_phase();
    let nest = scenario.add_real_card(P0, "Titans' Nest", Zone::Battlefield, db());
    let food = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let mut runner = scenario.build();

    activate_titans_nest(&mut runner, nest, food);
    assert_units_restricted(&runner, &titans_nest_restrictions(), 1);
    assert!(
        can_cast_object_now(runner.state(), P0, bears),
        "Grizzly Bears is green and has no {{X}}"
    );

    runner.cast(bears).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the Nest {{C}} paid the generic and the Forest paid {{G}}"
    );
    assert!(
        spent_sources(&runner, bears).contains(&nest),
        "Grizzly Bears' payment must name Titans' Nest"
    );
}

#[test]
fn titans_nest_colors_without_x_gate_rejects_colorless_and_x_spells() {
    // Colorless: Sol Ring has zero colors.
    let mut scenario = main_phase();
    let nest = scenario.add_real_card(P0, "Titans' Nest", Zone::Battlefield, db());
    let food = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    let mut runner = scenario.build();
    activate_titans_nest(&mut runner, nest, food);
    assert_units_restricted(&runner, &titans_nest_restrictions(), 1);
    assert!(
        !can_cast_object_now(runner.state(), P0, sol_ring),
        "a colorless spell has no colors"
    );

    // Control: a Forest pays Sol Ring's {1}; the Nest unit stays.
    let mut scenario = main_phase();
    let nest = scenario.add_real_card(P0, "Titans' Nest", Zone::Battlefield, db());
    let food = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Hand, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_titans_nest(&mut runner, nest, food);
    assert!(can_cast_object_now(runner.state(), P0, sol_ring));
    runner.cast(sol_ring).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "the Nest unit must remain unspent"
    );
    assert!(!spent_sources(&runner, sol_ring).contains(&nest));

    // {X}: Fireball's printed cost contains {X}, so it is ineligible even
    // though it is red. The engine reads the printed cost, per the card's
    // ruling that an {X} in an additional or alternative cost does not matter.
    let mut scenario = main_phase();
    let nest = scenario.add_real_card(P0, "Titans' Nest", Zone::Battlefield, db());
    let food = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    let fireball = scenario.add_real_card(P0, "Fireball", Zone::Hand, db());
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_titans_nest(&mut runner, nest, food);
    assert_units_restricted(&runner, &titans_nest_restrictions(), 1);

    // At X=0 Fireball costs just {R} and the Mountain pays it, so the offer
    // predicate cannot discriminate. X=1 is the honest probe: {1}{R} needs two
    // mana, the Mountain supplies {R}, and the Nest {C} must be withheld
    // because the printed cost contains {X}. The refusal therefore has to come
    // through the real cast pipeline at payment (CR 601.2f/h).
    let card_id = runner.state().objects[&fireball].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fireball,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announcing Fireball does not pay its cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "Fireball must announce X before any payment, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("X=1 is within the payable range at announcement");

    // CR 601.2c: choose the one target, then close the optional "any number"
    // slots exactly as the production client does.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 is a legal target for Fireball");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("the optional target slots must close");
    }
    // CR 601.2h: a Mountain (one {R}) plus the Nest's {C} cannot make {1}{R},
    // because that {C} is restricted to spells without {X}. Whether the single
    // target's division is a prompt or auto-split, the payment is attempted at
    // this boundary and must be refused.
    let refused = match runner.state().waiting_for.clone() {
        WaitingFor::DistributeAmong { targets, .. } => {
            assert_eq!(targets.len(), 1, "X=1 over one target");
            runner.act(GameAction::DistributeAmong {
                distribution: vec![(targets[0].clone(), 1)],
            })
        }
        WaitingFor::ManaPayment { .. } => runner.act(GameAction::PassPriority),
        other => panic!("Fireball must reach its payment boundary, got {other:?}"),
    };
    assert!(
        refused.is_err(),
        "with only a Mountain, the {{1}}{{R}} Fireball at X=1 is unpayable: \
         the Nest {{C}} may not pay for a spell with {{X}} in its printed cost"
    );
    assert_units_restricted(&runner, &titans_nest_restrictions(), 1);

    // Control: Forest + Mountain cast Fireball (X=1) with the Nest unit left.
    let mut scenario = main_phase();
    let nest = scenario.add_real_card(P0, "Titans' Nest", Zone::Battlefield, db());
    let food = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    let fireball = scenario.add_real_card(P0, "Fireball", Zone::Hand, db());
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db());
    let mut runner = scenario.build();
    activate_titans_nest(&mut runner, nest, food);
    assert!(can_cast_object_now(runner.state(), P0, fireball));
    runner.cast(fireball).x(1).target_player(P1).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "the Nest unit must remain unspent when the printed cost has {{X}}"
    );
    assert!(!spent_sources(&runner, fireball).contains(&nest));
}
