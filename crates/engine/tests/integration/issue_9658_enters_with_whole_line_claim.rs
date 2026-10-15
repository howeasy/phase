//! Issue #9658 — an "enters with" sentence is claimed as a replacement on the
//! object it is printed on only when its subject IS that object (or a
//! distributive subset of permanents). Lines whose subject another instruction
//! or a cast trigger supplies are not self replacements.
//!
//! Oracle text verified against MTGJSON `data/mtgjson/AtomicCards.json`:
//!   - Boreal Outrider: "Whenever you cast a creature spell, if {S} of any of
//!     that spell's colors was spent to cast it, that creature enters with an
//!     additional +1/+1 counter on it. ({S} is mana from a snow source.)"
//!   - Torgal, A Fine Hound: "Whenever you cast your first Human creature spell
//!     each turn, that creature enters with an additional +1/+1 counter on it for
//!     each Dog and/or Wolf you control." / "{T}: Add one mana of any color."
//!   - Hosting Season (three paragraphs, below).
//!   - Dark Intimations (two paragraphs, below).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1c: "[this permanent] enters with …" is a replacement effect.
//!   - CR 614.12: a replacement that modifies how a permanent enters applies to
//!     that permanent or a general subset of permanents.
//!   - CR 608.2c: instructions are followed in the order written; a later
//!     instruction refers to what an earlier one produced.
//!   - CR 113.6m: an ability whose effect moves the object it is on out of a
//!     zone functions only in that zone.
//!   - CR 113.7a: a triggered ability exists independently of its source once
//!     it has triggered.
//!   - CR 306.5b: a planeswalker's printed loyalty is its own entry
//!     replacement; an additional loyalty counter adds to it.
//!   - CR 616.1: the affected object's controller orders simultaneous
//!     replacement effects.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const BOREAL_OUTRIDER: &str = "Whenever you cast a creature spell, if {S} of any of that spell's colors was spent to cast it, that creature enters with an additional +1/+1 counter on it. ({S} is mana from a snow source.)";

const TORGAL: &str = "Whenever you cast your first Human creature spell each turn, that creature enters with an additional +1/+1 counter on it for each Dog and/or Wolf you control.\n{T}: Add one mana of any color.";

const HOSTING_SEASON: &str = "While it's October 25th, 2024, whenever you cast your commander, create a token that's a copy of it except it isn't legendary.\nWhile it's October 26th or 27th, 2024, if one or more tokens would be created under your control, instead twice that many tokens are created and each of them enters with a +1/+1 counter on it.\nOn any other date, both abilities apply. (Because why not?)";

const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

const DARK_INTIMATIONS: &str = "Each opponent sacrifices a creature or planeswalker of their choice, then discards a card. You return a creature or planeswalker card from your graveyard to your hand, then draw a card.\nWhen you cast a Bolas planeswalker spell, exile this card from your graveyard. That planeswalker enters with an additional loyalty counter on it.";

/// Control line: a plain self enters-with replacement.
const SELF_COUNTER: &str = "This creature enters with a +1/+1 counter on it.";

fn colorless(n: u32) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()))
        .collect()
}

fn generic_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic,
    }
}

/// Every effect reachable from `def` through sub and else branches.
fn chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut out = vec![def.effect.as_ref()];
    if let Some(sub) = def.sub_ability.as_deref() {
        out.extend(chain_effects(sub));
    }
    if let Some(else_branch) = def.else_ability.as_deref() {
        out.extend(chain_effects(else_branch));
    }
    out
}

fn all_effects(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    parsed
        .abilities
        .iter()
        .chain(parsed.triggers.iter().filter_map(|t| t.execute.as_deref()))
        .flat_map(chain_effects)
        .collect()
}

/// No counter-placing carrier of the rider anywhere on the card.
fn assert_no_counter_carrier(name: &str, parsed: &engine::parser::oracle::ParsedAbilities) {
    assert!(
        parsed.replacements.is_empty(),
        "{name}: no replacement may carry the rider, got {:?}",
        parsed.replacements
    );
    assert!(
        all_effects(parsed).iter().all(|effect| !matches!(
            effect,
            Effect::AddTargetReplacement { .. }
                | Effect::PutCounter { .. }
                | Effect::AddPendingETBCounters { .. }
        )),
        "{name}: no install, put or pending-counter carrier, got {parsed:?}"
    );
}

/// Cast a creature card from hand and return the counters it entered with.
fn cast_creature_and_count(name: &str, oracle: &str, cost: ManaCost, pool: Vec<ManaUnit>) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, name, 2, 2, oracle)
        .with_mana_cost(cost)
        .id();
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let entered = outcome
        .find_object(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("reach guard: {name} must resolve onto the battlefield"));
    outcome.counters(entered, CounterType::Plus1Plus1)
}

/// Boreal Outrider's rider names the creature spell its trigger watches, not
/// Boreal Outrider. It is an honest gap (the snow-colour payment metric does
/// not exist), and Boreal Outrider itself enters with no counter: the line is
/// never a `Moved`/`SelfRef` replacement on Boreal Outrider.
#[test]
fn boreal_outrider_is_an_honest_gap() {
    let parsed = parse_oracle_text(
        BOREAL_OUTRIDER,
        "Boreal Outrider",
        &[],
        &["Creature".to_string()],
        &["Elf".to_string(), "Warrior".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "reach guard: {parsed:?}");
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::SpellCast);
    assert!(
        matches!(
            trigger.execute.as_deref().map(|e| e.effect.as_ref()),
            Some(Effect::Unimplemented { .. })
        ),
        "the body is one explicit gap leaf, got {:?}",
        trigger.execute
    );
    assert_no_counter_carrier("Boreal Outrider", &parsed);

    let green_and_two = || {
        let mut pool = colorless(2);
        pool.push(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            Vec::new(),
        ));
        pool
    };
    let cost = || ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 2,
    };
    assert_eq!(
        cast_creature_and_count("Boreal Outrider", BOREAL_OUTRIDER, cost(), green_and_two()),
        0,
        "Boreal Outrider's text gives no counter to itself"
    );
    // Control: a real self enters-with line still gives its counter.
    assert_eq!(
        cast_creature_and_count("Test Grower", SELF_COUNTER, cost(), green_and_two()),
        1
    );
}

/// Torgal's rider names the Human creature spell its trigger watches. The
/// trigger head is read faithfully; the body is an honest gap until the floating
/// replacement can evaluate "Dogs and/or Wolves you control" for its installer.
/// Torgal itself enters with no counter (its text gives it none). This test
/// deliberately does not assert what a later Human gets, which would lock the
/// gap in as behaviour.
#[test]
fn torgal_is_an_honest_gap_and_does_not_count_itself() {
    let parsed = parse_oracle_text(
        TORGAL,
        "Torgal, A Fine Hound",
        &[],
        &["Creature".to_string()],
        &["Wolf".to_string()],
    );
    let triggers: Vec<_> = parsed
        .triggers
        .iter()
        .filter(|t| t.mode == TriggerMode::SpellCast)
        .collect();
    assert_eq!(triggers.len(), 1, "reach guard: {parsed:?}");
    assert!(
        matches!(
            triggers[0].constraint,
            Some(engine::types::ability::TriggerConstraint::NthSpellThisTurn { n: 1, .. })
        ),
        "the first-Human-spell head is read, got {:?}",
        triggers[0].constraint
    );
    assert!(
        matches!(
            triggers[0].execute.as_deref().map(|e| e.effect.as_ref()),
            Some(Effect::Unimplemented { .. })
        ),
        "the body is one explicit gap leaf, got {:?}",
        triggers[0].execute
    );
    assert!(
        parsed.replacements.is_empty(),
        "got {:?}",
        parsed.replacements
    );
    assert!(
        triggers[0]
            .execute
            .as_deref()
            .map(chain_effects)
            .unwrap_or_default()
            .iter()
            .all(|effect| !matches!(
                effect,
                Effect::AddTargetReplacement { .. }
                    | Effect::PutCounter { .. }
                    | Effect::AddPendingETBCounters { .. }
            )),
        "no install or put in the trigger body"
    );

    let cost = || ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    let pool = || {
        let mut pool = colorless(1);
        pool.push(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            Vec::new(),
        ));
        pool
    };
    assert_eq!(
        cast_creature_and_count("Torgal, A Fine Hound", TORGAL, cost(), pool()),
        0,
        "Torgal's text gives no counter to Torgal"
    );
    assert_eq!(
        cast_creature_and_count("Test Grower", SELF_COUNTER, cost(), pool()),
        1,
        "control"
    );
}

/// Hosting Season's second paragraph doubles created tokens and gives each of
/// THEM a counter; it is not a counter on Hosting Season. The line is not
/// claimed by the enters-with parser or by the token parser behind it, so it
/// stays an explicit gap, and Hosting Season enters with no counter.
#[test]
fn hosting_season_is_not_a_self_counter_replacement() {
    let parsed = parse_oracle_text(
        HOSTING_SEASON,
        "Hosting Season",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        parsed
            .replacements
            .iter()
            .all(|r| r.valid_card != Some(TargetFilter::SelfRef)
                && r.event != ReplacementEvent::CreateToken),
        "no self counter replacement and no partial token doubler, got {:?}",
        parsed.replacements
    );
    assert!(
        all_effects(&parsed)
            .iter()
            .all(|effect| !matches!(effect, Effect::AddTargetReplacement { .. })),
        "no floating doubler"
    );
    assert!(
        all_effects(&parsed).iter().any(|effect| effect
            .unimplemented_description()
            .is_some_and(|text| text.contains("each of them enters with"))),
        "reach guard: the doubling line is an explicit gap, got {parsed:?}"
    );

    // Control: Doubling Season's verbatim token line is still a token replacement.
    let doubling = parse_oracle_text(
        DOUBLING_SEASON,
        "Doubling Season",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(doubling
        .replacements
        .iter()
        .any(|r| r.event == ReplacementEvent::CreateToken));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let season = scenario
        .add_spell_to_hand(P0, "Hosting Season", false)
        .as_enchantment()
        .from_oracle_text(HOSTING_SEASON)
        .with_mana_cost(generic_cost(1))
        .id();
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();
    let outcome = runner.cast(season).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Hosting Season" && o.zone == Zone::Battlefield)
        .expect("reach guard: Hosting Season resolves onto the battlefield");
    assert_eq!(outcome.counters(entered, CounterType::Plus1Plus1), 0);
}

// ── Dark Intimations runtime (CR 113.6m + CR 113.7a + CR 608.2c + CR 306.5b) ──

const BOLAS_LOYALTY: u32 = 4;

fn add_dark_intimations(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    zone: Zone,
) -> ObjectId {
    let mut builder = match zone {
        Zone::Graveyard => scenario.add_spell_to_graveyard(owner, "Dark Intimations", false),
        Zone::Exile => scenario.add_spell_to_exile(owner, "Dark Intimations", false),
        Zone::Hand => scenario.add_spell_to_hand(owner, "Dark Intimations", false),
        other => panic!("unsupported fixture zone {other:?}"),
    };
    builder.from_oracle_text(DARK_INTIMATIONS).id()
}

/// A synthetic test planeswalker (no abilities) of the given subtype in hand,
/// printed loyalty `loyalty`, mana value 1. `flash` lets it be cast while
/// another spell is on the stack.
fn add_test_planeswalker(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
    flash: bool,
) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(owner, name, 0, 0, if flash { "Flash" } else { "" })
        .as_planeswalker_with_loyalty(subtype, loyalty)
        .with_mana_cost(generic_cost(1))
        .id()
}

fn loyalty_of(runner_state: &engine::types::game_state::GameState, name: &str) -> u32 {
    let obj = runner_state
        .objects
        .values()
        .find(|o| o.name == name && o.zone == Zone::Battlefield)
        .unwrap_or_else(|| panic!("reach guard: {name} must be on the battlefield"));
    obj.counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

fn zone_of_named(state: &engine::types::game_state::GameState, id: ObjectId) -> Zone {
    state
        .objects
        .get(&id)
        .map(|o| o.zone)
        .expect("object exists")
}

fn make_active(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn cast_spell(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
}

/// Pass priority (draining trigger-ordering prompts) until the stack has `len` items.
fn pass_priority_until_stack_len(runner: &mut GameRunner, len: usize) {
    for _ in 0..64 {
        if runner.state().stack.len() == len {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected WaitingFor while pumping to stack length {len}: {other:?}"),
        }
    }
    panic!("stack never reached length {len}");
}

/// Dark Intimations in its owner's graveyard: casting a Bolas planeswalker
/// triggers it from the graveyard, the card is exiled, and the planeswalker
/// enters with its printed loyalty plus one. The line is a graveyard cast
/// trigger, never a self replacement on the sorcery.
#[test]
fn dark_intimations_exiles_itself_and_adds_a_loyalty_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();
    assert_graveyard_cast_trigger_registered(&runner, dark);
    let outcome = runner.cast(walker).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 1
    );
    assert_eq!(outcome.zone_of(dark), Zone::Exile, "the card exiles itself");
}

/// Reach guard: the card carries its SpellCast trigger, functioning from the
/// graveyard (CR 113.6m), so a "no trigger" outcome is the trigger's own
/// condition or zone declining, not a missing definition.
fn assert_graveyard_cast_trigger_registered(runner: &GameRunner, dark: ObjectId) {
    assert!(
        runner.state().objects[&dark]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|entry| entry.definition.mode == TriggerMode::SpellCast
                && entry.definition.trigger_zones == vec![Zone::Graveyard]),
        "reach guard: the graveyard SpellCast trigger is registered"
    );
}

/// Hostile: a non-Bolas planeswalker does not trigger it, and the card stays.
/// Control: a Bolas planeswalker cast afterwards in the same game does trigger
/// it, so the non-Bolas outcome is the subtype condition declining.
#[test]
fn dark_intimations_ignores_a_non_bolas_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Jace Walker",
        "Jace",
        BOLAS_LOYALTY,
        false,
    );
    let bolas = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();
    assert_graveyard_cast_trigger_registered(&runner, dark);
    let outcome = runner.cast(walker).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Jace Walker"),
        BOLAS_LOYALTY
    );
    assert_eq!(outcome.zone_of(dark), Zone::Graveyard);

    let outcome = runner.cast(bolas).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 1,
        "control: a Bolas planeswalker triggers the same card"
    );
    assert_eq!(outcome.zone_of(dark), Zone::Exile);
}

/// Hostile: CR 113.6m — the ability functions only from the graveyard. In hand
/// or in exile the card does nothing.
#[test]
fn dark_intimations_outside_the_graveyard_does_not_trigger() {
    for zone in [Zone::Hand, Zone::Exile] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dark = add_dark_intimations(&mut scenario, P0, zone);
        let walker = add_test_planeswalker(
            &mut scenario,
            P0,
            "Test Bolas Walker",
            "Bolas",
            BOLAS_LOYALTY,
            false,
        );
        scenario.with_mana_pool(P0, colorless(1));
        let mut runner = scenario.build();
        assert_graveyard_cast_trigger_registered(&runner, dark);
        let outcome = runner.cast(walker).resolve();
        assert_eq!(
            loyalty_of(outcome.state(), "Test Bolas Walker"),
            BOLAS_LOYALTY,
            "{zone:?}"
        );
        assert_eq!(outcome.zone_of(dark), zone);
    }
}

/// Hostile: one card, one trigger. Once exiled it cannot add a counter to a
/// second Bolas planeswalker.
#[test]
fn dark_intimations_applies_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let first = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas One",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    let second = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Two",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    let outcome = runner.cast(second).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas One"),
        BOLAS_LOYALTY + 1
    );
    assert_eq!(loyalty_of(outcome.state(), "Test Bolas Two"), BOLAS_LOYALTY);
}

/// Owner vs controller: "When you cast" is the card's owner (its controller in
/// the graveyard). An opponent casting a Bolas planeswalker does not trigger it;
/// the same cast by the card's owner does — whether that owner is P0 or P1.
#[test]
fn dark_intimations_triggers_only_for_its_owner() {
    // The opponent casts; the card stays and the planeswalker gets no extra counter.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P1,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P1, colorless(1));
    let mut runner = scenario.build();
    make_active(&mut runner, P1);
    let outcome = runner.cast(walker).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY
    );
    assert_eq!(outcome.zone_of(dark), Zone::Graveyard);

    // P1 owns the card and casts the planeswalker: it triggers for P1.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark_intimations(&mut scenario, P1, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P1,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P1, colorless(1));
    let mut runner = scenario.build();
    make_active(&mut runner, P1);
    let outcome = runner.cast(walker).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 1
    );
    assert_eq!(outcome.zone_of(dark), Zone::Exile);
}

/// Binding: the installed replacement belongs to the spell that triggered it.
/// Another Bolas planeswalker that enters after the install but before that
/// spell resolves gets nothing; the original spell still gets its counter.
#[test]
fn dark_intimations_binds_to_the_triggering_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let original = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Original",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    let interloper =
        add_test_planeswalker(&mut scenario, P0, "Test Bolas Interloper", "Bolas", 3, true);
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();

    cast_spell(&mut runner, original);
    // Stack: [original, trigger]. The trigger resolves and installs the replacement.
    pass_priority_until_stack_len(&mut runner, 1);
    // A second Bolas planeswalker in response; the card is already exiled, so it
    // does not trigger again.
    cast_spell(&mut runner, interloper);
    pass_priority_until_stack_len(&mut runner, 0);

    assert_eq!(loyalty_of(runner.state(), "Test Bolas Interloper"), 3);
    assert_eq!(
        loyalty_of(runner.state(), "Test Bolas Original"),
        BOLAS_LOYALTY + 1
    );
}

/// CR 113.7a + CR 608.2c: once triggered, the ability does not depend on its
/// source. The card leaving the graveyard before the trigger resolves turns the
/// exile into a no-op, but the next instruction still installs the counter.
#[test]
fn dark_intimations_leaving_before_resolution_still_adds_the_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();
    let mut commit = runner.cast(walker).commit();
    assert_eq!(
        commit.state().stack.len(),
        2,
        "reach guard: the trigger is on the stack"
    );
    engine::game::zones::move_to_zone(commit.state_mut(), dark, Zone::Hand, &mut Vec::new());
    let outcome = commit.resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 1
    );
    assert_eq!(
        zone_of_named(outcome.state(), dark),
        Zone::Hand,
        "the card that left the graveyard is a new object the exile cannot find"
    );
}

/// Multi-authority: two copies in the graveyard both trigger for one Bolas
/// planeswalker; both are exiled and each adds its own counter (two separate
/// "additional" replacement effects both apply). The two entry replacements are
/// simultaneous (CR 616.1); the harness answers the order prompt with the first
/// candidate each time, and the result does not depend on the order.
#[test]
fn two_dark_intimations_both_apply() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let second = add_dark_intimations(&mut scenario, P0, Zone::Graveyard);
    let walker = add_test_planeswalker(
        &mut scenario,
        P0,
        "Test Bolas Walker",
        "Bolas",
        BOLAS_LOYALTY,
        false,
    );
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();
    let outcome = runner.cast(walker).replacement_choice(0).resolve();
    assert_eq!(
        loyalty_of(outcome.state(), "Test Bolas Walker"),
        BOLAS_LOYALTY + 2
    );
    assert_eq!(outcome.zone_of(first), Zone::Exile);
    assert_eq!(outcome.zone_of(second), Zone::Exile);
}

/// Synthetic (no corpus member): a keyword-qualified distributive subject.
/// Derived: each creature you control with flying enters with one +1/+1
/// counter for each creature you control; a creature without flying gets none.
/// The line is a distributive replacement, not a `static_structure` gap.
#[test]
fn keyword_qualified_distributive_subject_counts_at_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(
        P0,
        "Test Aerie",
        "Each creature you control with flying enters with an additional +1/+1 counter on it for each creature you control.",
    );
    scenario.add_vanilla(P0, 1, 1);
    scenario.add_vanilla(P0, 1, 1);
    let flyer = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Flyer", 1, 1, "Flying")
        .with_mana_cost(generic_cost(1))
        .id();
    let walker = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Walker", 1, 1, "")
        .with_mana_cost(generic_cost(1))
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();
    let outcome = runner.cast(flyer).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Test Flyer" && o.zone == Zone::Battlefield)
        .expect("reach guard: the flyer resolves");
    assert_eq!(
        outcome.counters(entered, CounterType::Plus1Plus1),
        2,
        "one counter per creature you control as it enters"
    );
    let outcome = runner.cast(walker).resolve();
    let entered = outcome
        .find_object(|o| o.name == "Test Walker" && o.zone == Zone::Battlefield)
        .expect("reach guard: the non-flyer resolves");
    assert_eq!(outcome.counters(entered, CounterType::Plus1Plus1), 0);
}
