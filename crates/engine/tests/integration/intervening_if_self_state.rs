//! Whole-card runtime witnesses for the parser's intervening-if support on
//! "self state" conditions. Each card's trigger only fires when the printed
//! condition holds, driven end to end through the production pipeline
//! (`add_real_card` printed cards + `GameRunner` real player actions).
//!
//! Cards (verbatim Oracle text, from client/public/card-data.json):
//!   - Krond the Dawn-Clad: "Whenever Krond attacks, if it's enchanted, exile
//!     target permanent."
//!   - Warchanter Skald: "Whenever this creature becomes tapped, if it's
//!     enchanted or equipped, create a 2/1 red Dwarf Berserker creature token."
//!   - Kaito Shizuki: "At the beginning of your end step, if Kaito entered
//!     this turn, he phases out."
//!
//! CR references (all verified against docs/MagicCompRules.txt):
//!   - CR 603.4: an intervening "if" clause after a trigger condition is
//!     checked when the trigger event occurs — the ability triggers only if
//!     the condition is true then (and again on resolution).
//!   - CR 303.4b: the object an Aura is attached to is called enchanted.
//!   - CR 301.5a: the creature an Equipment is attached to is the "equipped
//!     creature" — being equipped is not being enchanted.
//!   - CR 508.1a + CR 508.1f: declaring attackers is a turn-based action and
//!     attacking causes the attacking creatures to become tapped.
//!   - CR 508.2: after attackers are declared, the active player gets priority.
//!   - CR 508.3a: "Whenever [a creature] attacks" triggers when that creature
//!     is declared as an attacker.
//!   - CR 513.1: the end step has no turn-based actions; once it begins, the
//!     active player gets priority.
//!   - CR 702.26b + CR 702.26d: a permanent that phases out is "phased out"
//!     and treated as though it does not exist, without changing zones.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::game_object::{AttachTarget, PhaseStatus};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityTag, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// The integration fixture must load — a skipped test proves nothing
/// (regenerate with `python3 scripts/gen-test-fixture.py`).
fn load_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn one_mana(kind: ManaType) -> ManaUnit {
    ManaUnit::new(kind, ObjectId(0), false, vec![])
}

/// Assert the CR 508.2 priority window after declaring attackers: the stack
/// is empty and the active player holds priority. This is the settled state
/// every attack witness below ends in.
fn settle_at_declare_attackers_priority(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty at the CR 508.2 window, got {:?}",
        runner.stack_names()
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "expected P0's post-declaration priority (CR 508.2), got {:?}",
        runner.state().waiting_for
    );
}

/// Assert the end-step priority window: the stack is empty and a player holds
/// priority (CR 513.1).
fn settle_at_end_step_priority(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty in the end step, got {:?}",
        runner.stack_names()
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected end-step priority (CR 513.1), got {:?}",
        runner.state().waiting_for
    );
}

/// Battlefield ids of the created Dwarf Berserker tokens (the Warchanter Skald
/// token is printed with that exact name).
fn dwarf_berserker_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Dwarf Berserker")
        .map(|object| object.id)
        .collect()
}

/// Position of the printed equip ability in an Equipment's ability list.
fn equip_ability_index(runner: &GameRunner, equipment: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&equipment)
        .expect("equipment object exists")
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Equip))
        .expect("the printed Equipment must carry an equip activated ability")
}

// ---------------------------------------------------------------------------
// Krond the Dawn-Clad
// ---------------------------------------------------------------------------

/// "Whenever Krond attacks, if it's enchanted, exile target permanent."
/// CR 603.4 + CR 508.3a + CR 303.4b: Krond (6/6 flying vigilance) is enchanted
/// by a printed Aura (Rancor, cast through the real cast pipeline and attached
/// by resolution), declared as an attacker, and the trigger fires: the exile
/// target prompt offers permanents of BOTH players (CR 603.4: the ability only
/// reached the stack because the "enchanted" condition was true), the chosen
/// opponent permanent is exiled, and the game settles in the CR 508.2 window.
#[test]
fn krond_attacks_while_enchanted_exiles_target_permanent() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let krond = scenario.add_real_card(P0, "Krond the Dawn-Clad", Zone::Battlefield, db);
    let rancor = scenario.add_real_card(P0, "Rancor", Zone::Hand, db);
    let bear = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, vec![one_mana(ManaType::Green)]);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Enchant Krond through the real cast: Rancor resolves and attaches.
    let outcome = runner.cast(rancor).target_object(krond).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Rancor must finish resolving, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.zone_of(rancor), Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&rancor]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(krond),
        "reach: Rancor must be attached to Krond, so Krond is enchanted (CR 303.4b)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(krond, AttackTarget::Player(P1))])
        .expect("Krond must be declarable as an attacker");

    // CR 603.4: the trigger fired, so it asks for its single exile target now.
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Krond's exile target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "Krond's controller chooses the exile target");
    assert_eq!(target_slots.len(), 1, "exactly one 'target permanent' slot");
    let legal_targets = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal_targets.contains(&TargetRef::Object(bear)),
        "permanents controlled by the opponent (P1) must be legal targets"
    );
    assert!(
        legal_targets.contains(&TargetRef::Object(krond)),
        "permanents controlled by the attacker (P0) must be legal targets"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choosing the opponent's permanent must be accepted");
    runner.advance_until_stack_empty();

    // The chosen permanent is exiled — exactly it, and nothing else.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Exile,
        "the chosen opponent permanent must be exiled"
    );
    let exiled: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Exile)
        .map(|object| object.id)
        .collect();
    assert_eq!(exiled, vec![bear], "the exile zone holds exactly the bear");
    settle_at_declare_attackers_priority(&runner);
}

/// Negative sibling: the same attack WITHOUT any attachment. CR 603.4 — the
/// "if it's enchanted" condition is false at the trigger event, so nothing
/// goes on the stack and nothing is exiled. Reach-guard: the attack really was
/// declared (CR 508.1a — Krond is recorded as an attacker of P1) and the game
/// sits in the CR 508.2 declare-attackers priority window with an empty stack.
#[test]
fn krond_attacks_without_attachment_exiles_nothing() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let krond = scenario.add_real_card(P0, "Krond the Dawn-Clad", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(krond, AttackTarget::Player(P1))])
        .expect("Krond must be declarable as an attacker");

    // Reach-guard: the attack was declared and combat proceeded past it.
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat state must exist after the declaration");
    assert!(
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == krond),
        "reach: Krond must be recorded as an attacker (CR 508.1a)"
    );

    settle_at_declare_attackers_priority(&runner);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "no permanent may be exiled by the unmet trigger"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|object| object.zone != Zone::Exile),
        "the exile zone must be empty"
    );
}

/// Control: an Equipment attached (CR 301.5a) does NOT make Krond enchanted
/// (CR 303.4b) — "enchanted" requires an Aura. Shuko is attached through its
/// printed equip {0} activation, then the same attack fires nothing. Reach
/// guards: Shuko really is attached, and the attack is declared.
#[test]
fn krond_equipped_attacker_is_not_enchanted_and_exiles_nothing() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let krond = scenario.add_real_card(P0, "Krond the Dawn-Clad", Zone::Battlefield, db);
    let shuko = scenario.add_real_card(P0, "Shuko", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Attach through the printed equip ability, not by hand (CR 702.6a).
    let equip_index = equip_ability_index(&runner, shuko);
    runner
        .activate(shuko, equip_index)
        .target_object(krond)
        .resolve();
    assert_eq!(
        runner.state().objects[&shuko].attached_to,
        Some(AttachTarget::Object(krond)),
        "reach: Shuko must be attached to Krond, so Krond is equipped (CR 301.5a)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(krond, AttackTarget::Player(P1))])
        .expect("the equipped Krond must be declarable as an attacker");

    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat state must exist after the declaration");
    assert!(
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == krond),
        "reach: Krond must be recorded as an attacker (CR 508.1a)"
    );

    settle_at_declare_attackers_priority(&runner);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "being equipped must not satisfy 'if it's enchanted' (CR 303.4b)"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|object| object.zone != Zone::Exile),
        "the exile zone must be empty"
    );
}

// ---------------------------------------------------------------------------
// Warchanter Skald
// ---------------------------------------------------------------------------

/// "Whenever this creature becomes tapped, if it's enchanted or equipped,
/// create a 2/1 red Dwarf Berserker creature token."
///
/// Positive, enchanted branch: a printed Aura (Rancor) is cast onto the Skald,
/// then the Skald attacks and becomes tapped (CR 508.1f). CR 603.4 + CR 303.4b:
/// the "enchanted or equipped" condition is true, so exactly one 2/1 red Dwarf
/// Berserker token is created for P0.
#[test]
fn warchanter_skald_becomes_tapped_while_enchanted_creates_one_dwarf_berserker() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skald = scenario.add_real_card(P0, "Warchanter Skald", Zone::Battlefield, db);
    let rancor = scenario.add_real_card(P0, "Rancor", Zone::Hand, db);
    scenario.with_mana_pool(P0, vec![one_mana(ManaType::Green)]);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Enchant the Skald through the real cast.
    let outcome = runner.cast(rancor).target_object(skald).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Rancor must finish resolving, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        runner.state().objects[&rancor]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(skald),
        "reach: Rancor must be attached to the Skald, so it is enchanted (CR 303.4b)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(skald, AttackTarget::Player(P1))])
        .expect("the Skald must be declarable as an attacker");
    assert!(
        runner.state().objects[&skald].tapped,
        "reach: attacking must have tapped the Skald (CR 508.1f)"
    );

    runner.advance_until_stack_empty();

    let tokens = dwarf_berserker_ids(&runner);
    assert_eq!(tokens.len(), 1, "exactly one Dwarf Berserker token");
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!(
        token.controller, P0,
        "the token is created for the trigger's controller"
    );
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.power, Some(2), "printed token power 2");
    assert_eq!(token.toughness, Some(1), "printed token toughness 1");
    assert_eq!(token.color, vec![ManaColor::Red], "printed token color red");
    assert!(
        token.card_types.core_types.contains(&CoreType::Creature),
        "the token is a creature"
    );
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Dwarf")
            && token
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Berserker"),
        "the token is a Dwarf Berserker"
    );
    settle_at_declare_attackers_priority(&runner);
}

/// Positive, equipped branch: the Skald is equipped with a printed Equipment
/// (Shuko, attached via its printed equip {0} activation, CR 301.5a), attacks,
/// becomes tapped, and the token is created.
#[test]
fn warchanter_skald_becomes_tapped_while_equipped_creates_one_dwarf_berserker() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skald = scenario.add_real_card(P0, "Warchanter Skald", Zone::Battlefield, db);
    let shuko = scenario.add_real_card(P0, "Shuko", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let equip_index = equip_ability_index(&runner, shuko);
    runner
        .activate(shuko, equip_index)
        .target_object(skald)
        .resolve();
    assert_eq!(
        runner.state().objects[&shuko].attached_to,
        Some(AttachTarget::Object(skald)),
        "reach: Shuko must be attached to the Skald, so it is equipped (CR 301.5a)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(skald, AttackTarget::Player(P1))])
        .expect("the equipped Skald must be declarable as an attacker");
    assert!(
        runner.state().objects[&skald].tapped,
        "reach: attacking must have tapped the Skald (CR 508.1f)"
    );

    runner.advance_until_stack_empty();

    let tokens = dwarf_berserker_ids(&runner);
    assert_eq!(tokens.len(), 1, "exactly one Dwarf Berserker token");
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!(
        token.controller, P0,
        "the token is created for the trigger's controller"
    );
    assert_eq!(token.power, Some(2), "printed token power 2");
    assert_eq!(token.toughness, Some(1), "printed token toughness 1");
    settle_at_declare_attackers_priority(&runner);
}

/// Negative sibling: a bare Skald attacks, becomes tapped (reach-guard,
/// CR 508.1f), and NOTHING happens — CR 603.4: the "if it's enchanted or
/// equipped" condition is false at the tap event, so no trigger reaches the
/// stack and no token is created.
#[test]
fn warchanter_skald_becomes_tapped_unattached_creates_no_token() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skald = scenario.add_real_card(P0, "Warchanter Skald", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(skald, AttackTarget::Player(P1))])
        .expect("the bare Skald must be declarable as an attacker");
    assert!(
        runner.state().objects[&skald].tapped,
        "reach: attacking must have tapped the Skald (CR 508.1f), so the tap \
         trigger event really happened"
    );

    settle_at_declare_attackers_priority(&runner);
    assert_eq!(
        dwarf_berserker_ids(&runner).len(),
        0,
        "the unmet trigger must create no Dwarf Berserker token"
    );
}

// ---------------------------------------------------------------------------
// Kaito Shizuki
// ---------------------------------------------------------------------------

/// How Kaito is on the battlefield for the end-step run.
enum KaitoEntry {
    /// Cast from hand this turn through the real cast pipeline.
    CastFromHandThisTurn,
    /// Pre-existing permanent: entered a previous turn (`add_real_card` models
    /// a card that entered on a prior turn, and `at_phase` sets turn 2).
    EnteredBeforeThisTurn,
}

/// Drive one Kaito board to the beginning of P0's end step and settle the
/// stack: P0 casts Kaito from hand (paying {1}{U}{B} from the scenario-funded
/// pool) or starts with him already on the battlefield, the turn machinery
/// advances (auto-declaring the empty attack declaration), and the run stops
/// in the end-step priority window (CR 513.1).
fn kaito_reaching_end_step(entry: KaitoEntry) -> (GameRunner, ObjectId) {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaito = match entry {
        KaitoEntry::CastFromHandThisTurn => {
            scenario.with_mana_pool(
                P0,
                vec![
                    one_mana(ManaType::Colorless),
                    one_mana(ManaType::Blue),
                    one_mana(ManaType::Black),
                ],
            );
            scenario.add_real_card(P0, "Kaito Shizuki", Zone::Hand, db)
        }
        KaitoEntry::EnteredBeforeThisTurn => {
            scenario.add_real_card(P0, "Kaito Shizuki", Zone::Battlefield, db)
        }
    };
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    if let KaitoEntry::CastFromHandThisTurn = entry {
        let outcome = runner.cast(kaito).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "the Kaito cast must finish resolving, got {:?}",
            outcome.final_waiting_for()
        );
        assert_eq!(outcome.zone_of(kaito), Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&kaito].loyalty,
            Some(3),
            "Kaito must enter with his printed starting loyalty 3"
        );
        assert_eq!(
            runner.state().objects[&kaito].entered_battlefield_turn,
            Some(runner.state().turn_number),
            "the real cast must record Kaito's battlefield entry on the current turn"
        );
    }

    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("the empty attack declaration must be accepted");
        runner.advance_to_end_step();
    }
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the run must reach P0's end step (CR 513.1)"
    );
    runner.advance_until_stack_empty();
    (runner, kaito)
}

/// Positive: "At the beginning of your end step, if Kaito entered this turn, he
/// phases out." CR 603.4 + CR 702.26b/702.26d: Kaito was cast from hand THIS
/// turn, so at the end step the trigger fires and he is phased out — still on
/// the battlefield zone-wise, but with phased-out status.
#[test]
fn kaito_shizuki_enters_this_turn_phases_out_at_his_end_step() {
    let (runner, kaito) = kaito_reaching_end_step(KaitoEntry::CastFromHandThisTurn);

    let kaito_object = runner.state().objects.get(&kaito).unwrap();
    assert_eq!(
        kaito_object.zone,
        Zone::Battlefield,
        "phasing out does not change zones (CR 702.26d)"
    );
    assert!(
        matches!(kaito_object.phase_status, PhaseStatus::PhasedOut { .. }),
        "Kaito must be phased out at his controller's end step (CR 702.26b)"
    );
    settle_at_end_step_priority(&runner);
}

/// Negative sibling: Kaito has been on the battlefield since a previous turn,
/// so at the end step the intervening-if is false (CR 603.4): no trigger goes
/// on the stack and he stays phased in (CR 702.26c). Reach-guards: the end step
/// really was reached, and `entered_battlefield_turn` — the field the
/// `SourceEnteredThisTurn` condition arm reads
/// (`conditions::eval_source_entered_this_turn`) — is not the current turn, so
/// the condition was evaluated false, not skipped. The same test then runs the
/// control (cast from hand this turn) and confirms that run DOES phase him
/// out, so the negative can fail for the right reason.
#[test]
fn kaito_shizuki_entered_a_previous_turn_stays_phased_in_at_his_end_step() {
    let (runner, kaito) = kaito_reaching_end_step(KaitoEntry::EnteredBeforeThisTurn);

    let kaito_object = runner.state().objects.get(&kaito).unwrap();
    assert_ne!(
        kaito_object.entered_battlefield_turn,
        Some(runner.state().turn_number),
        "reach: Kaito's battlefield entry turn must not be the current turn, \
         so the 'entered this turn' condition is false rather than untested"
    );
    assert_eq!(kaito_object.zone, Zone::Battlefield);
    assert!(
        matches!(kaito_object.phase_status, PhaseStatus::PhasedIn),
        "Kaito must stay phased in when he did not enter this turn (CR 702.26c)"
    );
    settle_at_end_step_priority(&runner);

    // Control run: the same scenario with the condition true phases him out.
    let (control, control_kaito) = kaito_reaching_end_step(KaitoEntry::CastFromHandThisTurn);
    let control_object = control.state().objects.get(&control_kaito).unwrap();
    assert!(
        matches!(control_object.phase_status, PhaseStatus::PhasedOut { .. }),
        "control: a Kaito that entered this turn must phase out at the end step"
    );
}

// ---------------------------------------------------------------------------
// Novice Knight (static, same attachment-state arm)
// ---------------------------------------------------------------------------

/// "Defender\nAs long as this creature is enchanted or equipped, it can attack
/// as though it didn't have defender."
/// CR 702.3b + CR 303.4b: a Novice Knight enchanted by a printed Aura (Rancor,
/// cast through the real cast pipeline) may be declared as an attacker.
#[test]
fn novice_knight_enchanted_can_attack_despite_defender() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario.add_real_card(P0, "Novice Knight", Zone::Battlefield, db);
    let rancor = scenario.add_real_card(P0, "Rancor", Zone::Hand, db);
    scenario.with_mana_pool(P0, vec![one_mana(ManaType::Green)]);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(rancor).target_object(knight).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&rancor]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(knight),
        "reach: Rancor must be attached to the Knight (CR 303.4b)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(knight, AttackTarget::Player(P1))])
        .expect("an enchanted Novice Knight can attack as though it didn't have defender");
    let combat = runner.state().combat.as_ref().expect("combat state");
    assert_eq!(
        combat
            .attackers
            .iter()
            .map(|attacker| attacker.object_id)
            .collect::<Vec<_>>(),
        vec![knight]
    );
    settle_at_declare_attackers_priority(&runner);
}

/// Equipped branch of the same disjunction, attached through Shuko's printed
/// equip {0} (CR 301.5a + CR 702.6a).
#[test]
fn novice_knight_equipped_can_attack_despite_defender() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario.add_real_card(P0, "Novice Knight", Zone::Battlefield, db);
    let shuko = scenario.add_real_card(P0, "Shuko", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let equip_index = equip_ability_index(&runner, shuko);
    runner
        .activate(shuko, equip_index)
        .target_object(knight)
        .resolve();
    assert_eq!(
        runner.state().objects[&shuko].attached_to,
        Some(AttachTarget::Object(knight)),
        "reach: Shuko must be attached to the Knight (CR 301.5a)"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(knight, AttackTarget::Player(P1))])
        .expect("an equipped Novice Knight can attack as though it didn't have defender");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat state")
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == knight),
        "the equipped Knight is recorded as an attacker"
    );
    settle_at_declare_attackers_priority(&runner);
}

/// Negative sibling: the bare Novice Knight keeps Defender (CR 702.3b), so
/// declaring it as an attacker is rejected. Reach-guard: the same declaration
/// is legal in the enchanted test above, so the rejection is caused by the
/// unmet condition and not by the scenario.
#[test]
fn novice_knight_bare_cannot_attack_because_of_defender() {
    let db = load_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario.add_real_card(P0, "Novice Knight", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.advance_to_combat();
    assert!(
        runner
            .declare_attackers(&[(knight, AttackTarget::Player(P1))])
            .is_err(),
        "a Novice Knight with no attachment has Defender and cannot attack (CR 702.3b)"
    );
}
