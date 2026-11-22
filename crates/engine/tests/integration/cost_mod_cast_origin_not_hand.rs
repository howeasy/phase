//! Whole-card runtime witnesses for the cast-origin cost-modification clause
//! "Spells you cast from anywhere other than your hand cost {N} less to cast."
//!
//! The parser used to drop the origin qualifier, so the static reduced every
//! spell its controller cast, hand casts included (`spell_filter: null`); the
//! static now carries the `Not(InZone(Hand))` spell filter. Each witness loads
//! the printed card through `add_real_card` and drives it through the public
//! `GameRunner` actions. CR 601.2f: the reduction enters the total cost of a
//! cast announced from a zone other than the hand; CR 601.2a + CR 400.11: a
//! cast moves the card from the zone it is in onto the stack, so the origin is
//! the zone the card is cast from. Every reduced cast is displayed with
//! `display_spell_cost` AND really cast with exactly the reduced mana in the
//! pool (any mis-charge leaves the pool non-empty and fails the test), and
//! every card has a hand-cast control that must pay its full printed cost.
//! CR 611.2a: the level-gated Class grants last as long as their printed
//! condition states.
//!
//! The suite reads the committed integration card fixture, which must be
//! regenerated from the refreshed card-data export after the parser change
//! (`python3 scripts/gen-test-fixture.py`) before these tests can run: the
//! committed fixture still holds the pre-fix parse of Advanced Reconstruction
//! (`spell_filter: null`) and does not cover Savvy Trader, Sage of the Beyond,
//! or Fortune Teller's Talent at all.

use engine::database::card_db::CardDatabase;
use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{CastingPermission, Duration, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// A `ManaCost` literal: `cost(1, vec![ManaCostShard::Green])` is `{1}{G}`.
fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Pool mana for the color letters in `colors` (W U B R G C), one unit each.
fn units(colors: &str) -> Vec<ManaUnit> {
    colors
        .chars()
        .map(|letter| {
            let color = match letter {
                'W' => ManaType::White,
                'U' => ManaType::Blue,
                'B' => ManaType::Black,
                'R' => ManaType::Red,
                'G' => ManaType::Green,
                'C' => ManaType::Colorless,
                other => panic!("no mana letter {other}"),
            };
            ManaUnit::new(color, ObjectId(0), false, vec![])
        })
        .collect()
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

/// How many mana units `player` still has in their pool.
fn pool_size(r: &GameRunner, player: PlayerId) -> usize {
    r.state().players[player.0 as usize]
        .mana_pool
        .units()
        .count()
}

/// Index of the printed `{cost}: Level N` bar on `source`'s ability list.
fn level_bar_index(r: &GameRunner, source: ObjectId, level: u8) -> usize {
    r.state().objects[&source]
        .abilities
        .iter()
        .position(
            |a| matches!(a.effect.as_ref(), Effect::SetClassLevel { level: l } if *l == level),
        )
        .unwrap_or_else(|| {
            panic!(
                "the printed card must carry its Level {level} bar, found {:?}",
                r.state().objects[&source].abilities
            )
        })
}

/// The run finished: nothing is on the stack and `player` holds priority (a
/// suspended resolution can leave the stack empty, so both are asserted).
fn assert_settled(r: &GameRunner, player: PlayerId, what: &str) {
    assert!(
        r.state().stack.is_empty(),
        "{what}: the stack is empty, found {:?}",
        r.stack_names()
    );
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::Priority { player: p } if *p == player),
        "{what}: P{} holds priority, found {:?}",
        player.0,
        r.state().waiting_for
    );
}

/// Pass priority until the stack is empty. Fails loudly on any prompt
/// the witness did not declare.
fn pass_until_stack_empty(r: &mut GameRunner, what: &str) {
    for _ in 0..40 {
        if r.state().stack.is_empty() {
            return;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(r.state_mut());
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|e| panic!("{what}: passing priority was rejected: {e:?}"));
            }
            other => panic!("{what}: unexpected prompt while the stack resolves: {other:?}"),
        }
    }
    panic!(
        "{what}: the stack never emptied, stuck at {:?}",
        r.state().waiting_for
    );
}

/// Pass priority until a triggered ability's target prompt appears.
fn pass_until_trigger_target_prompt(r: &mut GameRunner, what: &str) {
    for _ in 0..16 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            return;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|e| panic!("{what}: passing priority was rejected: {e:?}"));
            }
            other => panic!("{what}: unexpected prompt while the cast resolves: {other:?}"),
        }
    }
    panic!(
        "{what}: the enters trigger's target prompt never appeared, stuck at {:?}",
        r.state().waiting_for
    );
}

// ===== Savvy Trader =====

/// Savvy Trader ({3}{G}): "When this creature enters, exile target permanent
/// card from your graveyard. You may play that card for as long as it remains
/// exiled." + "Spells you cast from anywhere other than your hand cost {1}
/// less to cast."
///
/// CR 601.2f: the {1} reduction must enter the total cost of the exiled
/// Grizzly Bears ({1}{G} -> {G}, a cast from exile, CR 601.2a + CR 400.11)
/// but not of a Grizzly Bears cast from the hand, whose origin is the hand.
/// The pre-fix parse (no origin filter) reduced the hand cast too.
#[test]
fn savvy_trader_reduces_only_casts_from_outside_the_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: Savvy Trader {3}{G} + the exiled Bears {G} + the hand
    // Bears {1}{G}. Anything left over at the end exposes a mis-charged cost.
    scenario.with_mana_pool(P0, units("CCCCGGG"));
    let savvy = scenario.add_real_card(P0, "Savvy Trader", Zone::Hand, db());
    let hand_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    let gy_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    // A second permanent card in the graveyard keeps the target prompt open (the
    // engine auto-commits a unique legal target) and is the wrong choice.
    let gy_forest = scenario.add_real_card(P0, "Forest", Zone::Graveyard, db());
    // P1's printed flashback card, for the opponent-cost control only.
    let p1_looting = scenario.add_real_card(P1, "Faithless Looting", Zone::Graveyard, db());

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    // The Trader is still in the hand, so its own cast is unreduced (this also
    // pins the printed cost the pool was funded against).
    assert_eq!(
        display_spell_cost(r.state(), P0, savvy),
        Some(cost(3, vec![ManaCostShard::Green])),
        "the printed Savvy Trader costs {{3}}{{G}}"
    );
    let card_id = r.state().objects[&savvy].card_id;
    r.act(GameAction::CastSpell {
        object_id: savvy,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast Savvy Trader from hand for its full printed {3}{G}");

    // The enters trigger's target prompt, driven through the
    // public action: assert actor and options before answering.
    pass_until_trigger_target_prompt(&mut r, "Savvy Trader enters");
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = r.state().waiting_for.clone()
    else {
        panic!("Savvy Trader: the enters trigger's target prompt is open");
    };
    assert_eq!(
        player, P0,
        "P0 chooses the exile target, got prompt for {player:?}"
    );
    assert_eq!(
        target_slots.len(),
        1,
        "the exile clause has exactly one target slot"
    );
    let slot = &target_slots[0];
    assert!(!slot.optional, "the exile target is required, not optional");
    let mut offered = slot.legal_targets.clone();
    offered.sort_by_key(|target| format!("{target:?}"));
    let mut expected = vec![TargetRef::Object(gy_bears), TargetRef::Object(gy_forest)];
    expected.sort_by_key(|target| format!("{target:?}"));
    assert_eq!(
        offered, expected,
        "the offered targets are exactly the two permanent cards in P0's graveyard"
    );
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(gy_bears)),
    })
    .expect("answer the exile target through the public action");
    pass_until_stack_empty(&mut r, "Savvy Trader enters trigger");

    assert_eq!(
        zone_of(&r, savvy),
        Zone::Battlefield,
        "Savvy Trader resolved"
    );
    assert_eq!(
        zone_of(&r, gy_bears),
        Zone::Exile,
        "the trigger exiled the Bears"
    );
    assert_eq!(
        zone_of(&r, gy_forest),
        Zone::Graveyard,
        "the Forest, not chosen, stays in the graveyard"
    );
    assert!(
        r.state().objects[&gy_bears]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0
            )),
        "the exiled Bears carries the play-from-exile permission for P0, got {:?}",
        r.state().objects[&gy_bears].casting_permissions
    );

    // The exiled Bears ({1}{G}) costs {G}: a cast from exile is reduced.
    assert_eq!(
        display_spell_cost(r.state(), P0, gy_bears),
        Some(cost(0, vec![ManaCostShard::Green])),
        "the exiled Bears costs {{G}}: reduced by {{1}} as a cast from exile"
    );
    let exiled_cast = r.cast(gy_bears).resolve();
    assert_eq!(
        exiled_cast.zone_of(gy_bears),
        Zone::Battlefield,
        "the exiled Bears is castable for exactly {{G}}"
    );
    assert_eq!(
        pool_size(&r, P0),
        2,
        "the exile cast charged exactly {{G}}: one colorless and one green remain"
    );

    // Control: the same printed card cast from the hand pays full price.
    assert_eq!(
        display_spell_cost(r.state(), P0, hand_bears),
        Some(cost(1, vec![ManaCostShard::Green])),
        "a Grizzly Bears cast from hand keeps its full printed {{1}}{{G}}"
    );
    let hand_cast = r.cast(hand_bears).resolve();
    assert_eq!(
        hand_cast.zone_of(hand_bears),
        Zone::Battlefield,
        "the hand Bears is castable at full price"
    );

    // The opponent's own graveyard flashback is untouched (the static says
    // "spells YOU cast").
    assert_eq!(
        display_spell_cost(r.state(), P1, p1_looting),
        Some(cost(2, vec![ManaCostShard::Red])),
        "P1's Faithless Looting flashback {{2}}{{R}} is not reduced by P0's Trader"
    );

    assert_eq!(
        pool_size(&r, P0),
        0,
        "the pool funded exactly {{3}}{{G}} + {{G}} + {{1}}{{G}} and is now empty"
    );
    assert_settled(&r, P0, "Savvy Trader");
}

// ===== Sage of the Beyond =====

/// Sage of the Beyond ({5}{U}{U} 5/5, Flying, Foretell {4}{U}): "Spells you
/// cast from anywhere other than your hand cost {2} less to cast."
///
/// CR 601.2f: the printed Flashback cost of a Faithless Looting in the
/// graveyard ({2}{R}, a cast from the graveyard, CR 601.2a + CR 400.11) is
/// reduced to {R}, while a sorcery cast from the hand (Divination {2}{U})
/// pays its full printed cost. The pre-fix parse reduced the hand cast too.
#[test]
fn sage_of_the_beyond_reduces_the_flashback_cast_but_not_hand_casts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: the flashback {R} + the hand Divination {2}{U}.
    scenario.with_mana_pool(P0, units("RCCU"));
    let sage = scenario.add_real_card(P0, "Sage of the Beyond", Zone::Battlefield, db());
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Graveyard, db());
    let p1_looting = scenario.add_real_card(P1, "Faithless Looting", Zone::Graveyard, db());
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    // Library, top first: Looting draws two, then Divination draws two.
    let looting_draw_a = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let looting_draw_b = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _divination_draw_a = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    let _divination_draw_b = scenario.add_real_card(P0, "Plains", Zone::Library, db());

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    // The printed keywords ride on the object as printed data.
    let keywords = &r.state().objects[&sage].keywords;
    assert!(
        keywords.contains(&Keyword::Flying),
        "printed Flying must be present on the object: {keywords:?}"
    );
    assert!(
        keywords
            .iter()
            .any(|k| matches!(k, Keyword::Foretell(c) if *c == cost(4, vec![ManaCostShard::Blue]))),
        "printed Foretell {{4}}{{U}} must be present on the object: {keywords:?}"
    );

    // The cast from the graveyard: printed Flashback {2}{R}, reduced by {2}.
    assert_eq!(
        display_spell_cost(r.state(), P0, looting),
        Some(cost(0, vec![ManaCostShard::Red])),
        "the Faithless Looting flashback {{2}}{{R}} is reduced to {{R}} by the cast-origin static"
    );
    let flashback = r
        .cast(looting)
        .discard(&[looting_draw_a, looting_draw_b])
        .resolve();
    assert_eq!(
        flashback.zone_of(looting),
        Zone::Exile,
        "the flashback cast resolves and then exiles the card"
    );

    // Control: the same card type cast from the hand pays full price.
    assert_eq!(
        display_spell_cost(r.state(), P0, divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "a sorcery cast from hand pays its full printed {{2}}{{U}}"
    );
    let hand_cast = r.cast(divination).resolve();
    assert_eq!(
        hand_cast.zone_of(divination),
        Zone::Graveyard,
        "the hand Divination is castable at full price"
    );

    // The opponent's own graveyard flashback is untouched.
    assert_eq!(
        display_spell_cost(r.state(), P1, p1_looting),
        Some(cost(2, vec![ManaCostShard::Red])),
        "P1's Faithless Looting flashback {{2}}{{R}} is not reduced by P0's Sage"
    );

    assert_eq!(
        pool_size(&r, P0),
        0,
        "the pool funded exactly {{R}} + {{2}}{{U}} and is now empty"
    );
    assert_settled(&r, P0, "Sage of the Beyond");
}

// ===== Fortune Teller's Talent =====

/// Fortune Teller's Talent ({U} Enchantment — Class), level 3 clause: "Spells
/// you cast from anywhere other than your hand cost {2} less to cast." Level 2
/// first grants "As long as you've cast a spell this turn, you may play cards
/// from the top of your library."
///
/// CR 601.2f + CR 611.2a: at level 2 the top-of-library cast (CR 601.2a +
/// CR 400.11: the card moves from the library onto the stack) costs its full
/// printed {2}{U}; only the level 3 clause reduces it to {U}, while a hand
/// cast stays full price at both levels. The pre-fix parse reduced the hand
/// cast and the level-2 top cast too.
#[test]
fn fortune_tellers_talent_level_3_reduces_the_top_of_library_cast_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: Level 2 {3}{U} + hand Divination {2}{U} + Level 3 {2}{U}
    // + the top-of-library Divination {U}.
    scenario.with_mana_pool(P0, units("CCCCCCCUUUU"));
    let talent = scenario.add_real_card(P0, "Fortune Teller's Talent", Zone::Battlefield, db());
    let hand_divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    let p1_divination = scenario.add_real_card(P1, "Divination", Zone::Hand, db());
    // Library, top first: the hand Divination draws the two Bears, then the
    // Divination the level 2 window casts sits on top, then two basics.
    let drawn_bears_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db());
    let _drawn_bears_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db());
    let top_divination = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    let _top_cast_draw_a = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let _top_cast_draw_b = scenario.add_real_card(P0, "Forest", Zone::Library, db());

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    // The printed Class enters at level 1; both level bars are activated
    // through the real activation action, each paid with exact pool mana.
    assert_eq!(
        r.state().objects[&talent].class_level,
        Some(1),
        "the printed Class enters the battlefield at level 1"
    );
    let level_two_bar = level_bar_index(&r, talent, 2);
    r.activate(talent, level_two_bar).resolve();
    assert_eq!(
        r.state().objects[&talent].class_level,
        Some(2),
        "the {{3}}{{U}} bar must set level 2"
    );

    // A spell cast from the hand this turn unlocks level 2's window — and the
    // hand cast itself pays full price.
    assert_eq!(
        display_spell_cost(r.state(), P0, hand_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "at level 2 the hand Divination pays its full printed {{2}}{{U}}"
    );
    let hand_cast = r.cast(hand_divination).resolve();
    assert_eq!(
        hand_cast.zone_of(hand_divination),
        Zone::Graveyard,
        "the hand Divination is castable at full price"
    );

    // Control BEFORE level 3: the top-of-library cast is available (a spell
    // was cast this turn) but NOT reduced — the reduction is the level 3
    // clause, so the pre-fix parse fails this assertion.
    assert_eq!(
        display_spell_cost(r.state(), P0, top_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "at level 2 only, the top-of-library Divination costs the full {{2}}{{U}}"
    );

    let level_three_bar = level_bar_index(&r, talent, 3);
    r.activate(talent, level_three_bar).resolve();
    assert_eq!(
        r.state().objects[&talent].class_level,
        Some(3),
        "the {{2}}{{U}} bar must set level 3"
    );

    // At level 3 the cast from the top of the library is reduced to {U}.
    assert_eq!(
        display_spell_cost(r.state(), P0, top_divination),
        Some(cost(0, vec![ManaCostShard::Blue])),
        "at level 3 the top-of-library Divination costs {{U}}"
    );
    let top_cast = r.cast(top_divination).resolve();
    assert_eq!(
        top_cast.zone_of(top_divination),
        Zone::Graveyard,
        "the top-of-library Divination is castable for exactly {{U}}"
    );

    // Controls: a card now in the hand, and the opponent, both pay full price
    // with level 3 active.
    assert_eq!(
        display_spell_cost(r.state(), P0, drawn_bears_a),
        Some(cost(1, vec![ManaCostShard::Green])),
        "the drawn Grizzly Bears in hand pays its full printed {{1}}{{G}} at level 3"
    );
    assert_eq!(
        display_spell_cost(r.state(), P1, p1_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "P1's Divination pays its full printed {{2}}{{U}}"
    );

    assert_eq!(
        pool_size(&r, P0),
        0,
        "the pool funded exactly {{3}}{{U}} + {{2}}{{U}} + {{2}}{{U}} + {{U}} and is now empty"
    );
    assert_settled(&r, P0, "Fortune Teller's Talent");
}

// ===== Advanced Reconstruction =====

/// Advanced Reconstruction ({3}{R} Enchantment — Class). Level 1: "At the
/// beginning of your first main phase, mill a card, then exile a card from your
/// graveyard at random. You may play the exiled card this turn." Level 2:
/// "Whenever one or more cards leave your graveyard, this Class deals 2 damage
/// to each opponent." Level 3: "Spells you cast from anywhere other than your
/// hand cost {2} less to cast."
///
/// CR 601.2f + CR 611.2a: leveled to 3 through the real activation actions,
/// the turn's first main phase (CR 601.2a + CR 400.11: the trigger exiles the
/// milled card, and the cast moves it from exile onto the stack) lets P0 cast
/// the exiled Divination for {U} ({2}{U} minus {2}) while a Divination cast
/// from the hand pays the full {2}{U}; the level 2 trigger deals exactly 2
/// damage to P1 when the milled card leaves the graveyard.
#[test]
fn advanced_reconstruction_level_3_reduces_the_exiled_cast_and_level_2_deals_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding for the two level bars, {1}{R} each.
    scenario.with_mana_pool(P0, units("RRCC"));
    let ar = scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db());
    let hand_divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    let p1_divination = scenario.add_real_card(P1, "Divination", Zone::Hand, db());
    // Library, top first: the draw step takes the Island, the level 1 trigger
    // mills and exiles the Divination (the only graveyard card, so the random
    // exile is deterministic), and the two Divination casts draw the rest.
    let draw_step_card = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let milled = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    let _exiled_cast_draw_a = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _exiled_cast_draw_b = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    let _hand_cast_draw_a = scenario.add_real_card(P0, "Plains", Zone::Library, db());
    let _hand_cast_draw_b = scenario.add_real_card(P0, "Swamp", Zone::Library, db());

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    // Level up through the real activation actions, sorcery speed in the main
    // phase, each bar paid with exact pool mana.
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(1),
        "the printed Class enters the battlefield at level 1"
    );
    let level_two_bar = level_bar_index(&r, ar, 2);
    r.activate(ar, level_two_bar).resolve();
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(2),
        "the {{1}}{{R}} bar set level 2"
    );
    let level_three_bar = level_bar_index(&r, ar, 3);
    r.activate(ar, level_three_bar).resolve();
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(3),
        "the {{1}}{{R}} bar set level 3"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the two level bars charged exactly {{1}}{{R}} each"
    );

    // A fresh turn: the draw step takes the top card, then the level 1
    // trigger fires at the beginning of the first main phase.
    r.state_mut().turn_number = 3;
    r.state_mut().phase = Phase::Untap;
    r.state_mut().active_player = P0;
    r.state_mut().priority_player = P0;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let waiting = r.auto_advance_to_main_phase();
    assert_eq!(
        r.state().phase,
        Phase::PreCombatMain,
        "the turn reached its first main phase"
    );
    assert!(
        matches!(waiting, WaitingFor::Priority { player } if player == P0),
        "P0 holds priority with the trigger queued, got {waiting:?}"
    );
    assert_eq!(
        r.state().stack.len(),
        1,
        "the level 1 phase trigger is on the stack"
    );
    assert_eq!(
        zone_of(&r, draw_step_card),
        Zone::Hand,
        "the draw step took the top card"
    );
    assert_eq!(
        zone_of(&r, milled),
        Zone::Library,
        "the Divination is now the top card"
    );
    assert!(
        r.state().players[P0.0 as usize].graveyard.is_empty(),
        "reach guard: the graveyard is empty, so the random exile must pick the milled card"
    );
    assert_eq!(r.life(P1), 20, "reach guard: P1 starts the turn at 20 life");

    // Resolve the level 1 trigger, then the level 2 damage trigger its
    // graveyard exit queues behind it.
    pass_until_stack_empty(&mut r, "Advanced Reconstruction triggers");
    assert_eq!(
        zone_of(&r, milled),
        Zone::Exile,
        "the trigger milled the Divination and exiled it from the graveyard"
    );
    assert!(
        r.state().objects[&milled]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    granted_to,
                    ..
                } if *granted_to == P0
            )),
        "the exiled Divination carries the this-turn play permission, got {:?}",
        r.state().objects[&milled].casting_permissions
    );
    assert_eq!(
        r.life(P1),
        18,
        "level 2: the card leaving the graveyard deals exactly 2 damage to P1"
    );

    // Fund this turn's casts exactly: the exiled Divination {U} + the hand
    // Divination {2}{U}.
    for unit in units("CCUU") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, milled),
        Some(cost(0, vec![ManaCostShard::Blue])),
        "the exiled Divination ({{2}}{{U}}) costs {{U}} at level 3"
    );
    let exiled_cast = r.cast(milled).resolve();
    assert_eq!(
        exiled_cast.zone_of(milled),
        Zone::Graveyard,
        "the exiled Divination is castable for exactly {{U}}"
    );

    // Control: the same printed card cast from the hand pays full price.
    assert_eq!(
        display_spell_cost(r.state(), P0, hand_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "a Divination cast from hand pays its full printed {{2}}{{U}} at level 3"
    );
    let hand_cast = r.cast(hand_divination).resolve();
    assert_eq!(
        hand_cast.zone_of(hand_divination),
        Zone::Graveyard,
        "the hand Divination is castable at full price"
    );

    // The opponent's cost is untouched by the "you cast" static.
    assert_eq!(
        display_spell_cost(r.state(), P1, p1_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "P1's Divination pays its full printed {{2}}{{U}}"
    );

    assert_eq!(
        pool_size(&r, P0),
        0,
        "this turn's pool funded exactly {{U}} + {{2}}{{U}} and is now empty"
    );
    assert_eq!(r.life(P1), 18, "no further cards left the graveyard");
    assert_settled(&r, P0, "Advanced Reconstruction");
}
