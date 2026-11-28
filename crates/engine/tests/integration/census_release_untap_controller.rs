//! Runtime witnesses for the ten `wrong_player` census rows released from
//! `confirmed-misparses.toml` for the single-object "<target> doesn't untap during its
//! controller's next untap step" class.
//!
//! The census read the parsed `PlayerScope::Controller` as the effect's controller. At runtime
//! the scope is the *affected permanent's* controller (CR 502.3: only its controller's untap
//! step untaps it, and the engine prunes the lock at that same untap step —
//! `game/layers.rs::prune_controller_untap_step_effects`, called from
//! `game/turns.rs::untap_permanents`). Each witness below pins that reading at runtime.
//!
//! The existing evidence is narrower: `announced_counter_recipient_set.rs` pins only the
//! recipient of Stensia Innkeeper's restriction (on a synthetic board),
//! `block_trigger_cardinality.rs` uses a synthetic Wall for the same clause, and
//! `chandra_revolution_doesnt_untap_slot.rs` measures a synthetic spell's fan-out. The
//! printed-card witnesses below are new evidence for the full one-step timing on these faces.
//!
//! Every released face is the printed card loaded from the card database through
//! `add_real_card`, and its full printed Oracle text is asserted against the database face
//! before any behavior is measured ([`card`]), so a paraphrased or drifted face fails first.
//! Support cards are printed too; only the library fillers are synthetic no-rules cards, the
//! batch-6 library convention.
//!
//! Per witness the target sits on the opponent's (P1's) battlefield and the test measures:
//! 1. the targeted permanent stays tapped through P1's next untap step;
//! 2. it untaps in P1's following untap step;
//! 3. an untargeted, tapped control permanent of P1's untaps in the first of those untap
//!    steps — the lock is bound to the chosen object, not broadcast over the class.
//!
//! Abilities that tap the target themselves (Frost Trickster, White Dragon, Stensia Innkeeper,
//! Mana Skimmer) are asserted to have tapped it; abilities that only forbid
//! untapping (Elvish Hunter, House Guildmage, Fifty Feet of Rope, Kefnet's Monument, Fogwalker,
//! Dreamshackle Geist) start with the target tapped. Every target prompt has a second legal
//! target and is answered explicitly, and every prompt is asserted unconditionally.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// A printed card and the Oracle text the test relies on.
#[derive(Clone, Copy)]
struct Printed {
    name: &'static str,
    oracle: &'static str,
}

const DREAMSHACKLE_GEIST: Printed = Printed {
    name: "Dreamshackle Geist",
    oracle: "Flying\nAt the beginning of combat on your turn, choose up to one —\n• Tap target creature.\n• Target creature doesn't untap during its controller's next untap step.",
};
const ELVISH_HUNTER: Printed = Printed {
    name: "Elvish Hunter",
    oracle: "{1}{G}, {T}: Target creature doesn't untap during its controller's next untap step.",
};
const FIFTY_FEET_OF_ROPE: Printed = Printed {
    name: "Fifty Feet of Rope",
    oracle: "Climb Over — {T}: Target Wall can't block this turn.\nTie Up — {3}, {T}: Target creature doesn't untap during its controller's next untap step.\nRappel Down — {4}, {T}: Venture into the dungeon. Activate only as a sorcery. (Enter the first room or advance to the next room.)",
};
const FOGWALKER: Printed = Printed {
    name: "Fogwalker",
    oracle: "Skulk (This creature can't be blocked by creatures with greater power.)\nWhen this creature enters, target creature an opponent controls doesn't untap during its controller's next untap step.",
};
const FROST_TRICKSTER: Printed = Printed {
    name: "Frost Trickster",
    oracle: "Flying\nWhen this creature enters, tap target creature an opponent controls. That creature doesn't untap during its controller's next untap step.",
};
const HOUSE_GUILDMAGE: Printed = Printed {
    name: "House Guildmage",
    oracle: "{1}{U}, {T}: Target creature doesn't untap during its controller's next untap step.\n{2}{B}, {T}: Surveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)",
};
const KEFNETS_MONUMENT: Printed = Printed {
    name: "Kefnet's Monument",
    oracle: "Blue creature spells you cast cost {1} less to cast.\nWhenever you cast a creature spell, target creature an opponent controls doesn't untap during its controller's next untap step.",
};
const MANA_SKIMMER: Printed = Printed {
    name: "Mana Skimmer",
    oracle: "Flying\nWhenever this creature deals damage to a player, tap target land that player controls. That land doesn't untap during its controller's next untap step.",
};
const STENSIA_INNKEEPER: Printed = Printed {
    name: "Stensia Innkeeper",
    oracle: "When this creature enters, tap target land an opponent controls. That land doesn't untap during its controller's next untap step.",
};
const WHITE_DRAGON: Printed = Printed {
    name: "White Dragon",
    oracle: "Flying\nCold Breath — When this creature enters, tap target creature an opponent controls. That creature doesn't untap during its controller's next untap step.",
};

/// Places a printed card after checking its database face carries the Oracle text the test
/// relies on. `add_real_card` panics if the face is absent from the fixture.
fn card(s: &mut GameScenario, player: PlayerId, printed: Printed, zone: Zone) -> ObjectId {
    let face = db()
        .get_face_by_name(printed.name)
        .unwrap_or_else(|| panic!("{} missing from the fixture", printed.name));
    assert_eq!(
        face.oracle_text.as_deref(),
        Some(printed.oracle),
        "{} Oracle text",
        printed.name
    );
    s.add_real_card(player, printed.name, zone, db())
}

/// Places a printed support card whose text the test does not quote (a vanilla creature, a basic
/// land, or a keyword-only creature).
fn plain(s: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(player, name, zone, db())
}

/// Pool mana for every printed cost used below, so a cast or activation pays exactly its
/// printed cost.
fn pool() -> Vec<ManaUnit> {
    let mut kinds = Vec::new();
    for kind in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        kinds.extend([kind; 6]);
    }
    kinds.extend([ManaType::Colorless; 4]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// Two players, P0 acting in the precombat main phase with `pool()`.
fn table() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    scenario
}

/// Textless library fillers for both players. The turn walk crosses several draw steps; with
/// empty libraries a player would lose by drawing from an empty library (CR 104.3c).
fn stock(s: &mut GameScenario) {
    s.with_library_top(
        P0,
        &[
            "P0 filler 1",
            "P0 filler 2",
            "P0 filler 3",
            "P0 filler 4",
            "P0 filler 5",
            "P0 filler 6",
            "P0 filler 7",
            "P0 filler 8",
        ],
    );
    s.with_library_top(
        P1,
        &[
            "P1 filler 1",
            "P1 filler 2",
            "P1 filler 3",
            "P1 filler 4",
            "P1 filler 5",
            "P1 filler 6",
            "P1 filler 7",
            "P1 filler 8",
        ],
    );
}

fn is_tapped(r: &GameRunner, id: ObjectId) -> bool {
    r.state().objects[&id].tapped
}

fn tap(r: &mut GameRunner, id: ObjectId) {
    r.state_mut().objects.get_mut(&id).unwrap().tapped = true;
}

/// The legal targets of the slot the engine is asking about (cast-time, activation-time or
/// trigger-time prompt).
fn offered_targets(r: &GameRunner) -> Vec<ObjectId> {
    let (player, selection) = match &r.state().waiting_for {
        WaitingFor::TargetSelection {
            player, selection, ..
        }
        | WaitingFor::TriggerTargetSelection {
            player, selection, ..
        } => (*player, selection),
        other => panic!("expected a target prompt, got {other:?}"),
    };
    assert_eq!(player, P0, "P0 announces the targets");
    let mut ids: Vec<ObjectId> = selection
        .current_legal_targets
        .iter()
        .map(|t| match t {
            TargetRef::Object(o) => *o,
            TargetRef::Player(p) => panic!("an object prompt offered player {p:?}"),
        })
        .collect();
    ids.sort_by_key(|o| o.0);
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|o| o.0);
    ids
}

fn choose_object(r: &mut GameRunner, id: ObjectId) {
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(id)),
    })
    .expect("choose target");
}

fn cast(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast the printed card");
}

fn activate(r: &mut GameRunner, source: ObjectId, index: usize) {
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate the printed ability");
}

/// The index, in the object's ability list, of its `n`th activated ability. Printed abilities
/// keep their text order, so this names the printed ability without hard-coding a slot.
fn activated_index(r: &GameRunner, id: ObjectId, n: usize) -> usize {
    r.state().objects[&id]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| ability.cost.is_some())
        .map(|(index, _)| index)
        .nth(n)
        .unwrap_or_else(|| panic!("the card has no activated ability #{n}"))
}

fn assert_settled(r: &GameRunner, what: &str) {
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "{what}: ends at priority, got {:?}",
        r.state().waiting_for
    );
    assert!(r.state().stack.is_empty(), "{what}: the stack is empty");
}

/// Answer the pending target prompt (asserting the exact offered set) and pass priority until
/// the stack is empty and priority returns. Every witness has two legal targets, so the engine
/// cannot auto-select and the chosen recipient is exactly what the test names (CR 601.2c /
/// CR 603.3d).
fn resolve_pending(r: &mut GameRunner, what: &str, offered: &[ObjectId], chosen: ObjectId) {
    for _ in 0..40 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection { player, .. }
            | WaitingFor::TriggerTargetSelection { player, .. } => {
                assert_eq!(player, P0, "{what}: P0 chooses the target");
                assert_eq!(
                    offered_targets(r),
                    sorted(offered.to_vec()),
                    "{what}: the offered targets"
                );
                choose_object(r, chosen);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .unwrap_or_else(|err| panic!("{what}: ordering triggers: {err:?}"));
            }
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|err| panic!("{what}: pass priority: {err:?}"));
            }
            other => panic!("{what}: unexpected prompt {other:?}"),
        }
    }
    panic!("{what}: the stack did not settle");
}

/// Pass priority until the stack empties without ever asking for a target. Used where the
/// trigger's referent is an untargeted event object (CR 608.2k), so a target prompt here would
/// itself be the defect.
fn settle(r: &mut GameRunner, what: &str) {
    for _ in 0..40 {
        match r.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .unwrap_or_else(|err| panic!("{what}: ordering triggers: {err:?}"));
            }
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|err| panic!("{what}: pass priority: {err:?}"));
            }
            other => panic!("{what}: unexpected prompt {other:?}"),
        }
    }
    panic!("{what}: the stack did not settle");
}

/// Drive the engine with default answers (no attacks, no blocks, pass priority) until it reaches
/// `phase` holding priority with an empty stack.
fn drive_to_priority(r: &mut GameRunner, phase: Phase, what: &str) {
    for _ in 0..400 {
        if r.state().phase == phase
            && r.state().stack.is_empty()
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = match r.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::AssignCombatDamage {
                total_damage,
                ref blockers,
                ..
            } => GameAction::AssignCombatDamage {
                mode: Default::default(),
                assignments: vec![(blockers[0].blocker_id, total_damage)],
                trample_damage: 0,
                controller_damage: 0,
            },
            // Default answer to a repeating "choose up to one" trigger met while walking
            // turns: choose no mode (CR 603.3c / CR 700.2b removes the trigger). The card
            // under test is measured before this point; the walk only needs the phase to move.
            WaitingFor::AbilityModeChoice { ref modal, .. } if modal.min_choices == 0 => {
                GameAction::SelectModes { indices: vec![] }
            }
            WaitingFor::OrderTriggers { ref triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!(
                "{what}: unexpected prompt {other:?} in {:?} (turn {})",
                r.state().phase,
                r.state().turn_number
            ),
        };
        r.act(action)
            .unwrap_or_else(|err| panic!("{what}: {err:?}"));
    }
    panic!("{what}: never reached {phase:?}");
}

/// Advance until `player`'s untap step has just run. The first priority window after an untap
/// step is that turn's upkeep (CR 502.4: nobody receives priority during the untap step), and
/// `advance_to_phase` returns immediately when it is already in the target phase, so the walk
/// goes through an intervening end step instead of asking for the untap step directly.
fn advance_to_next_untap_of(r: &mut GameRunner, player: PlayerId, what: &str) {
    for _ in 0..6 {
        drive_to_priority(r, Phase::End, what);
        drive_to_priority(r, Phase::Upkeep, what);
        if r.state().active_player == player {
            return;
        }
    }
    panic!("{what}: never reached {player:?}'s untap step");
}

/// CR 502.3: the lock spans exactly the affected permanent's next untap step, and only that one
/// step. The untargeted control permanent of the same controller untaps in that same step, so
/// the restriction is bound to the chosen object rather than broadcast over the class.
fn assert_locked_one_untap_step(
    r: &mut GameRunner,
    player: PlayerId,
    locked: ObjectId,
    control: ObjectId,
    what: &str,
) {
    advance_to_next_untap_of(r, player, what);
    assert!(
        is_tapped(r, locked),
        "{what}: the targeted permanent stays tapped through its controller's next untap step"
    );
    assert!(
        !is_tapped(r, control),
        "{what}: the untargeted control permanent untaps in that same untap step"
    );
    advance_to_next_untap_of(r, player, what);
    assert!(
        !is_tapped(r, locked),
        "{what}: the lock spans exactly one untap step of the affected permanent's controller"
    );
    assert!(
        !is_tapped(r, control),
        "{what}: the control permanent stays untapped"
    );
}

/// Pass priority to the beginning of combat, assert the printed modal trigger's mode prompt
/// (source, triggered, "choose up to one", two printed modes), and choose the mode whose printed
/// bullet contains `needle`. Selecting the mode by its printed text — not by a hard-coded index —
/// keeps the witness honest about which mode it exercises.
fn choose_trigger_mode(r: &mut GameRunner, source: ObjectId, needle: &str, what: &str) -> usize {
    for _ in 0..40 {
        match r.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice {
                source_id,
                modal,
                is_activated,
                ..
            } => {
                assert_eq!(
                    source_id, source,
                    "{what}: the mode prompt belongs to the card"
                );
                assert!(!is_activated, "{what}: the prompt is a triggered ability");
                assert_eq!(modal.min_choices, 0, "{what}: 'choose up to one' has min 0");
                assert_eq!(modal.max_choices, 1, "{what}: 'choose up to one' has max 1");
                assert_eq!(
                    modal.mode_descriptions.len(),
                    2,
                    "{what}: both printed modes are offered: {:?}",
                    modal.mode_descriptions
                );
                let index = modal
                    .mode_descriptions
                    .iter()
                    .position(|text| text.contains(needle))
                    .unwrap_or_else(|| {
                        panic!(
                            "{what}: no mode description contains {needle:?}: {:?}",
                            modal.mode_descriptions
                        )
                    });
                r.act(GameAction::SelectModes {
                    indices: vec![index],
                })
                .unwrap_or_else(|err| panic!("{what}: choose mode {index}: {err:?}"));
                return index;
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|err| panic!("{what}: pass priority: {err:?}"));
            }
            other => panic!("{what}: expected the mode prompt, got {other:?}"),
        }
    }
    panic!("{what}: no mode prompt");
}

// =============================================================================================
// "Target creature doesn't untap during its controller's next untap step." — activated
// =============================================================================================

/// Elvish Hunter ({1}{G}, {T}): the restriction binds the chosen creature; the untargeted
/// second creature untaps normally in P1's next untap step.
#[test]
fn elvish_hunter_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let hunter = card(&mut scenario, P0, ELVISH_HUNTER, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    let index = activated_index(&r, hunter, 0);
    activate(&mut r, hunter, index);
    resolve_pending(&mut r, "Elvish Hunter", &[hunter, target, control], target);
    assert!(is_tapped(&r, hunter), "the {{T}} cost tapped Elvish Hunter");
    assert!(is_tapped(&r, target), "the target is still tapped");
    assert!(is_tapped(&r, control), "the control is still tapped");

    assert_locked_one_untap_step(&mut r, P1, target, control, "Elvish Hunter");
    assert_settled(&r, "Elvish Hunter");
}

/// House Guildmage ({1}{U}, {T}): the first ability is the untap lock; the surveil ability shares
/// the card and must not run.
#[test]
fn house_guildmage_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let mage = card(&mut scenario, P0, HOUSE_GUILDMAGE, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    let index = activated_index(&r, mage, 0);
    activate(&mut r, mage, index);
    resolve_pending(&mut r, "House Guildmage", &[mage, target, control], target);
    assert!(is_tapped(&r, mage), "the {{T}} cost tapped House Guildmage");
    assert!(is_tapped(&r, target) && is_tapped(&r, control));

    assert_locked_one_untap_step(&mut r, P1, target, control, "House Guildmage");
    assert_settled(&r, "House Guildmage");
}

/// Fifty Feet of Rope — Tie Up ({3}, {T}): the second printed activated ability is the lock. The
/// ability's own target filter is "Target creature", so both bears are offered and the choice is
/// explicit.
#[test]
fn fifty_feet_of_rope_tie_up_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let rope = card(&mut scenario, P0, FIFTY_FEET_OF_ROPE, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    let index = activated_index(&r, rope, 1);
    activate(&mut r, rope, index);
    resolve_pending(&mut r, "Fifty Feet of Rope", &[target, control], target);
    assert!(is_tapped(&r, rope), "the {{T}} cost tapped the Rope");
    assert!(is_tapped(&r, target) && is_tapped(&r, control));

    assert_locked_one_untap_step(&mut r, P1, target, control, "Fifty Feet of Rope");
    assert_settled(&r, "Fifty Feet of Rope");
}

// =============================================================================================
// "When this creature enters, target creature an opponent controls doesn't untap …" — ETB
// =============================================================================================

/// Fogwalker (ETB): the trigger targets only an opponent's creature, so Fogwalker itself is not
/// offered; the chosen bear is locked, the other untaps.
#[test]
fn fogwalker_etb_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let fogwalker = card(&mut scenario, P0, FOGWALKER, Zone::Hand);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    cast(&mut r, fogwalker);
    resolve_pending(&mut r, "Fogwalker ETB", &[target, control], target);
    assert_eq!(
        r.state().objects[&fogwalker].zone,
        Zone::Battlefield,
        "reach: Fogwalker resolved"
    );
    assert!(is_tapped(&r, target) && is_tapped(&r, control));

    assert_locked_one_untap_step(&mut r, P1, target, control, "Fogwalker");
    assert_settled(&r, "Fogwalker");
}

// =============================================================================================
// "When this creature enters, tap target creature an opponent controls. That creature doesn't
// untap during its controller's next untap step." — ETB with a printed tap
// =============================================================================================

/// Frost Trickster (ETB): the trigger taps the chosen creature itself, so an untapped target
/// proves the tap clause ran; it then stays tapped through P1's next untap step while the
/// pre-tapped control untaps.
#[test]
fn frost_trickster_etb_taps_and_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let trickster = card(&mut scenario, P0, FROST_TRICKSTER, Zone::Hand);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, control);

    cast(&mut r, trickster);
    resolve_pending(&mut r, "Frost Trickster ETB", &[target, control], target);
    assert_eq!(
        r.state().objects[&trickster].zone,
        Zone::Battlefield,
        "reach: Frost Trickster resolved"
    );
    assert!(
        is_tapped(&r, target),
        "the trigger's tap clause tapped the chosen creature"
    );
    assert!(is_tapped(&r, control), "the control was already tapped");

    assert_locked_one_untap_step(&mut r, P1, target, control, "Frost Trickster");
    assert_settled(&r, "Frost Trickster");
}

/// White Dragon ("Cold Breath", ETB): same printed shape, so the same non-vacuous tap probe.
#[test]
fn white_dragon_etb_taps_and_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let dragon = card(&mut scenario, P0, WHITE_DRAGON, Zone::Hand);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, control);

    cast(&mut r, dragon);
    resolve_pending(&mut r, "White Dragon ETB", &[target, control], target);
    assert_eq!(
        r.state().objects[&dragon].zone,
        Zone::Battlefield,
        "reach: White Dragon resolved"
    );
    assert!(
        is_tapped(&r, target),
        "the trigger's tap clause tapped the chosen creature"
    );
    assert!(is_tapped(&r, control), "the control was already tapped");

    assert_locked_one_untap_step(&mut r, P1, target, control, "White Dragon");
    assert_settled(&r, "White Dragon");
}

/// Stensia Innkeeper (ETB): the target is a *land* an opponent controls. The trigger taps the
/// chosen land; it stays tapped through P1's next untap step while the pre-tapped control land
/// untaps.
#[test]
fn stensia_innkeeper_etb_taps_and_locks_only_the_targeted_land() {
    let mut scenario = table();
    let innkeeper = card(&mut scenario, P0, STENSIA_INNKEEPER, Zone::Hand);
    let target = plain(&mut scenario, P1, "Forest", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Forest", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, control);

    cast(&mut r, innkeeper);
    resolve_pending(&mut r, "Stensia Innkeeper ETB", &[target, control], target);
    assert_eq!(
        r.state().objects[&innkeeper].zone,
        Zone::Battlefield,
        "reach: Stensia Innkeeper resolved"
    );
    assert!(
        is_tapped(&r, target),
        "the trigger's tap clause tapped the chosen land"
    );
    assert!(
        is_tapped(&r, control),
        "the control land was already tapped"
    );

    assert_locked_one_untap_step(&mut r, P1, target, control, "Stensia Innkeeper");
    assert_settled(&r, "Stensia Innkeeper");
}

// =============================================================================================
// "Whenever you cast a creature spell, target creature an opponent controls doesn't untap …"
// =============================================================================================

/// Kefnet's Monument: casting a printed creature spell fires the cast trigger, which locks the
/// chosen opponent creature. The cast creature is P0's, so only P1's bears are offered.
#[test]
fn kefnets_monument_cast_trigger_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let monument = card(&mut scenario, P0, KEFNETS_MONUMENT, Zone::Battlefield);
    let bears = plain(&mut scenario, P0, "Grizzly Bears", Zone::Hand);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    cast(&mut r, bears);
    resolve_pending(
        &mut r,
        "Kefnet's Monument cast trigger",
        &[target, control],
        target,
    );
    assert_eq!(
        r.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach: the triggering creature spell resolved"
    );
    assert!(
        r.state().objects[&monument].zone == Zone::Battlefield,
        "reach: Kefnet's Monument is still the trigger's source"
    );
    assert!(is_tapped(&r, target) && is_tapped(&r, control));

    assert_locked_one_untap_step(&mut r, P1, target, control, "Kefnet's Monument");
    assert_settled(&r, "Kefnet's Monument");
}

// =============================================================================================
// "Whenever this creature deals damage to a player, tap target land that player controls. That
// land doesn't untap during its controller's next untap step." — combat-damage trigger
// =============================================================================================

/// Mana Skimmer deals combat damage to P1; the damage trigger taps the chosen P1 land, which
/// then skips P1's next untap step. The pre-tapped control land untaps in that same step.
#[test]
fn mana_skimmer_damage_trigger_taps_and_locks_only_the_targeted_land() {
    let mut scenario = table();
    let skimmer = card(&mut scenario, P0, MANA_SKIMMER, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Forest", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Forest", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, control);

    r.advance_to_combat();
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        ),
        "the declare-attackers prompt, got {:?}",
        r.state().waiting_for
    );
    r.declare_attackers(&[(skimmer, AttackTarget::Player(P1))])
        .expect("attack with Mana Skimmer");
    let outcome = r.combat_damage();
    outcome.assert_life_delta(P1, -2);

    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the damage trigger asks for its land target, got {:?}",
        r.state().waiting_for
    );
    assert_eq!(
        offered_targets(&r),
        sorted(vec![target, control]),
        "only the damaged player's lands are offered"
    );
    choose_object(&mut r, target);
    settle(&mut r, "Mana Skimmer trigger");
    assert!(
        is_tapped(&r, target),
        "the trigger's tap clause tapped the chosen land"
    );
    assert!(
        is_tapped(&r, control),
        "the control land was already tapped"
    );

    assert_locked_one_untap_step(&mut r, P1, target, control, "Mana Skimmer");
    assert_settled(&r, "Mana Skimmer");
}

// =============================================================================================
// "At the beginning of combat on your turn, choose up to one — • Tap target creature.
// • Target creature doesn't untap during its controller's next untap step." — modal trigger
// =============================================================================================

/// Dreamshackle Geist, untap mode (index 1). The mode prompt is asserted (source, triggered,
/// min 0 / max 1), then the target prompt: "Target creature" includes the Geist itself, so the
/// offered set is all three creatures and the choice is explicit.
#[test]
fn dreamshackle_geist_untap_mode_locks_only_the_targeted_creature() {
    let mut scenario = table();
    let geist = card(&mut scenario, P0, DREAMSHACKLE_GEIST, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, target);
    tap(&mut r, control);

    choose_trigger_mode(&mut r, geist, "doesn't untap", "Dreamshackle Geist");
    resolve_pending(
        &mut r,
        "Dreamshackle Geist untap mode",
        &[geist, target, control],
        target,
    );
    assert!(is_tapped(&r, target) && is_tapped(&r, control));

    assert_locked_one_untap_step(&mut r, P1, target, control, "Dreamshackle Geist");
    assert_settled(&r, "Dreamshackle Geist");
}

/// Control: the Geist's other mode (index 0, "Tap target creature") taps the chosen creature but
/// installs no untap lock, so that creature untaps in P1's next untap step. Fails if the untap
/// mode's restriction were installed regardless of the chosen mode.
#[test]
fn dreamshackle_geist_tap_mode_does_not_lock_the_targeted_creature() {
    let mut scenario = table();
    let geist = card(&mut scenario, P0, DREAMSHACKLE_GEIST, Zone::Battlefield);
    let target = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    let control = plain(&mut scenario, P1, "Grizzly Bears", Zone::Battlefield);
    stock(&mut scenario);
    let mut r = scenario.build();
    tap(&mut r, control);

    choose_trigger_mode(
        &mut r,
        geist,
        "Tap target creature",
        "Dreamshackle Geist tap mode",
    );
    resolve_pending(
        &mut r,
        "Dreamshackle Geist tap mode",
        &[geist, target, control],
        target,
    );
    assert!(
        is_tapped(&r, target),
        "the tap mode's tap clause tapped the chosen creature"
    );
    assert!(is_tapped(&r, control), "the control was already tapped");

    advance_to_next_untap_of(&mut r, P1, "Dreamshackle Geist tap mode");
    assert!(
        !is_tapped(&r, target),
        "the tap mode leaves no untap lock, so the tapped creature untaps"
    );
    assert!(
        !is_tapped(&r, control),
        "the untargeted control creature untaps in the same step"
    );
    assert_settled(&r, "Dreamshackle Geist tap mode");
}
