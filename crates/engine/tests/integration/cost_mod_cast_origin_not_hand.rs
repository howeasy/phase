//! Whole-card runtime witnesses for the cast-origin cost-modification clause
//! "Spells you cast from anywhere other than your hand cost {N} less to cast."
//!
//! The parser used to drop the origin qualifier, so the static reduced every
//! spell its controller cast, hand casts included (`spell_filter: null`); the
//! static now excludes only the static controller's own hand (CR 400.3: that
//! hand is identified by the card's owner, not by what "opponent" means). Each
//! witness loads the printed card through `add_real_card` and drives it through
//! the public `GameRunner` actions. CR 601.2f: the reduction enters the total
//! cost of a cast announced from anywhere other than that hand; CR 601.2a: a
//! cast moves the card from the zone it is in onto the stack, so its origin is
//! the zone the card is cast from. The witnesses check displayed costs and
//! actual payment, including a free cast that owes a tax before reductions.
//!
//! The suite parses a small raw-MTGJSON fixture
//! (`fixtures/cost_mod_cast_origin_atomic.json`: the printed cards of the class
//! plus the witness cards and basic lands) through
//! `CardDatabase::from_mtgjson`, so the parser change is on the tested path.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::casting::display_spell_cost;
use engine::game::combat::AttackTarget;
use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{CastingPermission, Duration, Effect, TargetRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::create_card_copy_with_chosen_name::garth_choose;

/// The printed cards from a small raw-MTGJSON fixture, so every witness runs the
/// production parser on the real Oracle text: reverting the parser fix reverts
/// these cards' parse.
fn db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        CardDatabase::from_mtgjson(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/cost_mod_cast_origin_atomic.json"),
        )
        .expect("the raw MTGJSON fixture parses")
    })
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
        // CR 603.3b: drain the per-controller ordering prompt with identity
        // BEFORE the stack-empty check — the prompt can surface mid-resolution
        // with an empty stack while triggers wait to be dispatched. Same order
        // as `GameRunner::advance_until_stack_empty`.
        if matches!(r.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(r.state_mut());
            continue;
        }
        if r.state().stack.is_empty() {
            return;
        }
        match r.state().waiting_for.clone() {
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

/// Advance the turn structure with public actions (priority passes, empty
/// attack/block declarations, trigger-order drains) until P0's next
/// precombat main after turn `after_turn` opens with P0 holding priority and
/// nothing on the stack. Fails loudly on any prompt the witness did not
/// declare.
fn advance_to_next_p0_main(r: &mut GameRunner, after_turn: u32, what: &str) {
    for _ in 0..800 {
        {
            let state = r.state();
            if state.active_player == P0
                && state.phase == Phase::PreCombatMain
                && state.turn_number > after_turn
                && state.stack.is_empty()
                && state.deferred_triggers.is_empty()
                && matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0)
            {
                return;
            }
        }
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .unwrap_or_else(|e| panic!("{what}: passing priority was rejected: {e:?}"));
            }
            WaitingFor::DeclareAttackers { .. } => {
                r.act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers through the public action");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.declare_blockers(&[])
                    .expect("declare no blockers through the public action");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(r.state_mut());
            }
            other => {
                panic!("{what}: unexpected prompt while advancing to P0's next main: {other:?}")
            }
        }
    }
    panic!(
        "{what}: never reached P0's next precombat main, stuck at {:?}",
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
/// Grizzly Bears ({1}{G} -> {G}, a cast from exile, CR 601.2a)
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

/// Savvy Trader ({3}{G}): "When this creature enters, exile target permanent
/// card from your graveyard. You may play that card for as long as it
/// remains exiled."
///
/// CR 601.2f + CR 601.2a: the printed permission is NOT a this-turn
/// permission. This turn's cast of the exiled Grizzly Bears is declined;
/// after the turn structure is crossed through public actions, the Bears —
/// still exiled — is castable on a later P0 turn for {G} ({1}{G} reduced by
/// the printed {1}, a cast whose origin is exile), really cast with exactly
/// that mana. The later-turn hand-cast control is the revert-failing
/// assertion: a Grizzly Bears cast from the hand on that same later turn pays
/// its full printed {1}{G}.
#[test]
fn savvy_trader_play_permission_lasts_while_the_card_remains_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: Savvy Trader {3}{G}; the Bears cast is funded on the
    // later turn (the pool empties at the end of every step and phase).
    scenario.with_mana_pool(P0, units("CCCG"));
    let savvy = scenario.add_real_card(P0, "Savvy Trader", Zone::Hand, db());
    let gy_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    // A second permanent card in the graveyard keeps the target prompt open
    // (the engine auto-commits a unique legal target).
    let gy_forest = scenario.add_real_card(P0, "Forest", Zone::Graveyard, db());
    // Later-turn hand-cast control: the same printed card from the hand pays
    // its full price even though the exiled copy is reduced.
    let hand_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db());
    // Draw-step filler for the turn cycle the witness crosses.
    scenario.with_library_top(P0, &["Island"; 2]);
    scenario.with_library_top(P1, &["Forest"; 2]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    let card_id = r.state().objects[&savvy].card_id;
    r.act(GameAction::CastSpell {
        object_id: savvy,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast Savvy Trader from hand for its full printed {3}{G}");

    pass_until_trigger_target_prompt(&mut r, "Savvy Trader enters");
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = r.state().waiting_for.clone()
    else {
        panic!("the enters trigger's target prompt is open");
    };
    assert_eq!(
        player, P0,
        "P0 chooses the exile target, got the prompt for {player:?}"
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

    // The cast is DECLINED this turn: the Bears was castable (display {G})
    // but P0 spends nothing and leaves it in exile.
    assert_eq!(
        display_spell_cost(r.state(), P0, gy_bears),
        Some(cost(0, vec![ManaCostShard::Green])),
        "this turn the exiled Bears is already castable for {{G}}"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the cast was declined: nothing was spent this turn"
    );

    // A later P0 turn, reached through public actions only.
    let declined_turn = r.state().turn_number;
    advance_to_next_p0_main(&mut r, declined_turn, "Savvy Trader later turn");
    assert!(
        r.state().turn_number > declined_turn,
        "a full turn cycle was crossed"
    );
    assert_eq!(
        zone_of(&r, gy_bears),
        Zone::Exile,
        "the Bears is still exiled"
    );
    assert!(
        r.state().objects[&gy_bears]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0
            )),
        "the play-from-exile permission survived the turn boundary, got {:?}",
        r.state().objects[&gy_bears].casting_permissions
    );
    for unit in units("GGC") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, gy_bears),
        Some(cost(0, vec![ManaCostShard::Green])),
        "on a later turn the still-exiled Bears costs {{G}}: {{1}}{{G}} reduced by the printed {{1}}"
    );
    let later_cast = r.cast(gy_bears).resolve();
    assert_eq!(
        later_cast.zone_of(gy_bears),
        Zone::Battlefield,
        "the Bears is castable from exile on the later turn for exactly {{G}}"
    );

    // The later-turn hand-cast control: the same printed card from the hand
    // keeps its full printed {1}{G}, so the reduction above is the origin
    // exclusion and not a blanket reduction.
    assert_eq!(
        display_spell_cost(r.state(), P0, hand_bears),
        Some(cost(1, vec![ManaCostShard::Green])),
        "a Grizzly Bears cast from hand on the later turn keeps its full printed {{1}}{{G}}"
    );
    let later_hand_cast = r.cast(hand_bears).resolve();
    assert_eq!(
        later_hand_cast.zone_of(hand_bears),
        Zone::Battlefield,
        "the later-turn hand Bears is castable at full price"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the later-turn casts charged exactly {{G}} + {{1}}{{G}}"
    );
    assert_settled(&r, P0, "Savvy Trader later turn");
}

// ===== Savvy Trader: casting from an opponent's hand =====

fn oni_thalia_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("CCCU"));
    let oni = scenario.add_real_card(P0, "Silent-Blade Oni", Zone::Battlefield, db());
    scenario.add_real_card(P0, "Thalia, Guardian of Thraben", Zone::Battlefield, db());
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db());
    let own_divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    let opponent_divination = scenario.add_real_card(P1, "Divination", Zone::Hand, db());
    for land in ["Island", "Forest", "Mountain", "Plains"] {
        scenario.add_real_card(P0, land, Zone::Library, db());
    }
    scenario.add_real_card(P1, "Swamp", Zone::Library, db());
    (scenario, oni, island, own_divination, opponent_divination)
}

fn cast_thalias_taxed_hand_divination(r: &mut GameRunner, divination: ObjectId) {
    assert_eq!(
        display_spell_cost(r.state(), P0, divination),
        Some(cost(3, vec![ManaCostShard::Blue])),
        "P0's own-hand Divination costs its printed {{2}}{{U}} plus Thalia's {{1}}"
    );
    let card_id = r.state().objects[&divination].card_id;
    r.act(GameAction::CastSpell {
        object_id: divination,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast P0's own-hand Divination");
    pass_until_stack_empty(r, "P0's hand Divination");
    assert_eq!(zone_of(r, divination), Zone::Graveyard);
    assert_eq!(
        pool_size(r, P0),
        0,
        "the hand cast spent exactly {{3}}{{U}}"
    );
    assert_settled(r, P0, "P0's hand Divination");
}

fn select_opponents_divination_with_oni(r: &mut GameRunner, oni: ObjectId, divination: ObjectId) {
    let life_before = r.life(P1);
    r.advance_to_combat();
    r.declare_attackers(&[(oni, AttackTarget::Player(P1))])
        .expect("declare Oni attacking P1");
    for _ in 0..40 {
        match r.state().waiting_for {
            WaitingFor::OptionalEffectChoice { player: P0, .. } => break,
            WaitingFor::DeclareBlockers { player: P1, .. } => {
                r.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("pass through combat to Oni's trigger");
            }
            ref other => panic!("unexpected prompt before Oni's choice: {other:?}"),
        }
    }
    assert_eq!(r.life(P1), life_before - 6, "the unblocked Oni hit P1");
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "P0 must receive Oni's optional cast prompt"
    );
    r.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Oni's cast offer");
    assert!(
        matches!(
            &r.state().waiting_for,
            WaitingFor::EffectZoneChoice { player: P0, zone: Zone::Hand, cards, .. }
                if cards.contains(&divination)
        ),
        "Oni must offer P1's Divination from hand, got {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::SelectCards {
        cards: vec![divination],
    })
    .expect("select P1's Divination through Oni");
}

fn assert_oni_divination_on_stack(r: &GameRunner, divination: ObjectId) {
    assert_eq!(zone_of(r, divination), Zone::Stack);
    assert_eq!(r.state().objects[&divination].owner, P1);
    assert!(
        r.state()
            .stack
            .iter()
            .any(|entry| entry.source_id == divination && entry.controller == P0),
        "P0 casts the spell that P1 owns"
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast must be complete before passing priority: {:?}",
        r.state().waiting_for
    );
}

/// CR 118.9d + CR 601.2f: Thalia adds {1} to Oni's alternative {0} cost,
/// then Savvy removes {1} because P0 is casting from P1's hand.
#[test]
fn savvy_trader_reduces_a_spell_cast_from_the_opponents_hand() {
    let (mut scenario, oni, island, own_divination, opponent_divination) = oni_thalia_scenario();
    scenario.add_real_card(P0, "Savvy Trader", Zone::Battlefield, db());
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    cast_thalias_taxed_hand_divination(&mut r, own_divination);
    assert!(!r.state().objects[&island].tapped);

    let hand_before = r.state().players[0].hand.len();
    select_opponents_divination_with_oni(&mut r, oni, opponent_divination);
    assert!(
        !r.state().objects[&island].tapped,
        "Savvy must cancel Thalia's tax on the cast from P1's hand"
    );
    assert_oni_divination_on_stack(&r, opponent_divination);
    assert_eq!(pool_size(&r, P0), 0);
    pass_until_stack_empty(&mut r, "Oni's Divination");
    assert_eq!(zone_of(&r, opponent_divination), Zone::Graveyard);
    assert_eq!(r.state().players[0].hand.len(), hand_before + 2);
    assert_settled(&r, P0, "Oni's Divination");
}

#[test]
fn silent_blade_oni_free_cast_from_the_opponents_hand_pays_thalias_tax_without_savvy() {
    let (scenario, oni, island, own_divination, opponent_divination) = oni_thalia_scenario();
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    cast_thalias_taxed_hand_divination(&mut r, own_divination);
    assert!(!r.state().objects[&island].tapped);

    select_opponents_divination_with_oni(&mut r, oni, opponent_divination);
    if matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ) {
        let tap = activatable_mana_actions_for_player(r.state(), P0)
            .into_iter()
            .find(|action| {
                matches!(action, GameAction::TapLandForMana { selection }
                    if selection.source.object_id == island)
            })
            .expect("the Island must be available to pay Thalia's tax");
        r.act(tap).expect("tap the Island to pay Thalia's tax");
        if matches!(
            r.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ) {
            r.act(GameAction::PassPriority)
                .expect("pay Thalia's tax from the mana pool");
        }
    }
    assert_oni_divination_on_stack(&r, opponent_divination);
    assert!(
        r.state().objects[&island].tapped,
        "without Savvy the only available mana must pay Thalia's {{1}}"
    );
    assert_eq!(pool_size(&r, P0), 0);
}

// ===== Sage of the Beyond =====

/// Sage of the Beyond ({5}{U}{U} 5/5, Flying, Foretell {4}{U}): "Spells you
/// cast from anywhere other than your hand cost {2} less to cast."
///
/// CR 601.2f: the printed Flashback cost of a Faithless Looting in the
/// graveyard ({2}{R}, a cast from the graveyard, CR 601.2a) is
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

// ===== Sage of the Beyond: its own Foretell =====

/// Sage of the Beyond ({5}{U}{U} 5/5, Flying, Foretell {4}{U}): "Spells you
/// cast from anywhere other than your hand cost {2} less to cast."
///
/// The Sage's own printed Foretell is the natural non-hand cast. The {2}
/// special action exiles a second Sage copy from the hand face down, and on a
/// later P0 turn the foretold copy is cast from exile (CR 601.2a: the cast
/// moves the card from the zone it is in — exile — onto the stack) for its
/// foretell cost {4}{U} minus {2} = {2}{U} (CR 601.2f). The control scenario
/// has no Sage on the battlefield: the same printed cast then costs the full
/// foretell cost {4}{U}, so the reduction belongs to the battlefield Sage's
/// printed static. The printed Flying is witnessed by its use: P1's ground
/// Grizzly Bears is offered no block assignment against the attacking Sage,
/// and the unblocked Sage deals its printed 5 combat damage.
#[test]
fn sage_of_the_beyond_reduces_its_own_foretell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding for the {2} foretell special action; the later cast is
    // funded on the turn it happens (the pool empties at each step).
    scenario.with_mana_pool(P0, units("CC"));
    let sage = scenario.add_real_card(P0, "Sage of the Beyond", Zone::Battlefield, db());
    let foretold_copy = scenario.add_real_card(P0, "Sage of the Beyond", Zone::Hand, db());
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db());
    // Draw-step filler for the turn cycle the witness crosses.
    scenario.with_library_top(P0, &["Island"; 2]);
    scenario.with_library_top(P1, &["Forest"; 2]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    // Both printed objects carry the printed keywords.
    let printed_foretell = cost(4, vec![ManaCostShard::Blue]);
    for id in [sage, foretold_copy] {
        let keywords = &r.state().objects[&id].keywords;
        assert!(
            keywords.contains(&Keyword::Flying),
            "printed Flying must be present on {id:?}: {keywords:?}"
        );
        assert!(
            keywords
                .iter()
                .any(|k| matches!(k, Keyword::Foretell(c) if *c == printed_foretell)),
            "printed Foretell {{4}}{{U}} must be present on {id:?}: {keywords:?}"
        );
    }

    // The {2} foretell special action, paid from the pool through the public
    // action: the copy leaves the hand face down and is stamped foretold.
    let foretell_turn = r.state().turn_number;
    let card_id = r.state().objects[&foretold_copy].card_id;
    r.act(GameAction::Foretell {
        object_id: foretold_copy,
        card_id,
    })
    .expect("the {2} foretell special action pays from the pool and exiles the copy");
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::Priority { player } if *player == P0),
        "the foretell payment completed without a prompt, got {:?}",
        r.state().waiting_for
    );
    assert_eq!(
        zone_of(&r, foretold_copy),
        Zone::Exile,
        "the foretell action exiled the copy"
    );
    {
        let foretold = &r.state().objects[&foretold_copy];
        assert!(foretold.foretold, "the exiled copy is marked foretold");
        assert!(foretold.face_down, "the exiled copy is face down");
        assert!(
            foretold.casting_permissions.iter().any(|permission| matches!(
                permission,
                CastingPermission::Foretold { turn_foretold, .. } if *turn_foretold == foretell_turn
            )),
            "the exiled copy carries the Foretold permission stamped with turn {foretell_turn}, got {:?}",
            foretold.casting_permissions
        );
    }
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the foretell special action charged exactly {{2}}"
    );

    // Printed Flying, witnessed by its use: the attacking Sage is unblockable
    // by P1's ground Bears. The declare-blockers prompt publishes the exact
    // block assignments P1 may declare, and the Bears is offered none.
    r.advance_to_combat();
    let WaitingFor::DeclareAttackers {
        player,
        valid_attacker_ids,
        ..
    } = r.state().waiting_for.clone()
    else {
        panic!("the declare-attackers prompt is open");
    };
    assert_eq!(
        player, P0,
        "P0 declares the attack, got the prompt for {player:?}"
    );
    assert!(
        valid_attacker_ids.contains(&sage),
        "the Sage is a legal attacker, offered {valid_attacker_ids:?}"
    );
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(sage, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("declare the Sage attacking P1");
    // P1's only creature is a ground Bears: against a flier it has no legal block,
    // so the engine never opens a declare-blockers prompt (had the Sage lost Flying
    // the Bears would be offered the block). Pass priority through combat and
    // fail on any prompt the witness did not declare.
    assert_eq!(
        zone_of(&r, bears),
        Zone::Battlefield,
        "reach guard: the Bears is on P1's battlefield and able to block a ground attacker"
    );
    for _ in 0..24 {
        if r.state().phase == Phase::PostCombatMain {
            break;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("pass priority through the combat steps");
            }
            other => panic!(
                "Sage of the Beyond attacks: a prompt opened although the Bears cannot block a flier: {other:?}"
            ),
        }
    }
    assert_eq!(
        r.state().phase,
        Phase::PostCombatMain,
        "combat ended without a block prompt"
    );
    assert_eq!(
        r.life(P1),
        15,
        "the unblocked flying Sage dealt its printed 5 combat damage"
    );

    // A later P0 turn, reached through public actions only.
    advance_to_next_p0_main(&mut r, foretell_turn, "Sage of the Beyond foretell cast");
    assert!(
        r.state().turn_number > foretell_turn && r.state().active_player == P0,
        "the foretold copy is cast on a later P0 turn"
    );
    for unit in units("CCU") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, foretold_copy),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "the foretold Sage's foretell cost {{4}}{{U}} is reduced by {{2}} as a cast from exile"
    );
    let foretell_cast = r.cast(foretold_copy).resolve();
    assert_eq!(
        foretell_cast.zone_of(foretold_copy),
        Zone::Battlefield,
        "the foretold Sage is castable for exactly {{2}}{{U}}"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the foretell cast charged exactly {{2}}{{U}}"
    );
    assert_settled(&r, P0, "Sage of the Beyond foretell cast");

    // Control: without a Sage on the battlefield the same printed foretell
    // cast costs the full foretell cost — the {2} reduction comes from the
    // battlefield Sage's printed static, not from the foretell path itself.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("CC"));
    let lone_copy = scenario.add_real_card(P0, "Sage of the Beyond", Zone::Hand, db());
    scenario.with_library_top(P0, &["Island"; 2]);
    scenario.with_library_top(P1, &["Forest"; 2]);
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    let control_foretell_turn = r.state().turn_number;
    let card_id = r.state().objects[&lone_copy].card_id;
    r.act(GameAction::Foretell {
        object_id: lone_copy,
        card_id,
    })
    .expect("foretell the copy with no Sage on the battlefield");
    advance_to_next_p0_main(&mut r, control_foretell_turn, "Sage of the Beyond control");
    for unit in units("CCCCU") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, lone_copy),
        Some(cost(4, vec![ManaCostShard::Blue])),
        "without the battlefield Sage the foretold Sage costs its full foretell cost {{4}}{{U}}"
    );
    let control_cast = r.cast(lone_copy).resolve();
    assert_eq!(
        control_cast.zone_of(lone_copy),
        Zone::Battlefield,
        "the control cast is castable for exactly {{4}}{{U}}"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the control cast charged the full {{4}}{{U}}"
    );
    assert_settled(&r, P0, "Sage of the Beyond control");
}

// ===== Bilbo, Thief in the Night (static only) =====

/// Bilbo, Thief in the Night ({1}{U} 2/2): "Spells you cast from anywhere other
/// than your hand cost {1} less to cast." + "Whenever Bilbo attacks, you may
/// cast an artifact, instant, or sorcery spell from your graveyard. If an
/// instant or sorcery spell cast this way would be put into your graveyard,
/// exile it instead."
///
/// STATIC-ONLY witness: this test claims only the printed STATIC. Bilbo's attack
/// trigger and its exile-instead replacement are NOT witnessed, and his census
/// row (class `missing_effect`, second clause only) stays.
///
/// CR 601.2f: the printed Flashback cost of a Faithless Looting in the
/// graveyard ({2}{R}, a cast from the graveyard, CR 601.2a) is reduced to
/// {1}{R}, while a Divination cast from the hand pays its full printed {2}{U}.
/// The pre-fix parse (no origin filter) reduced the hand cast too.
#[test]
fn bilbo_static_reduces_graveyard_cast_not_hand_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: the flashback {1}{R} + the hand Divination {2}{U}.
    scenario.with_mana_pool(P0, units("CCCRU"));
    let _bilbo = scenario.add_real_card(P0, "Bilbo, Thief in the Night", Zone::Battlefield, db());
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

    // The graveyard cast: printed Flashback {2}{R}, reduced by {1}.
    assert_eq!(
        display_spell_cost(r.state(), P0, looting),
        Some(cost(1, vec![ManaCostShard::Red])),
        "the Faithless Looting flashback {{2}}{{R}} is reduced to {{1}}{{R}} by Bilbo's static"
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

    // Control: a sorcery cast from the hand pays full price.
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

    // The opponent's own graveyard flashback is untouched (the static says
    // "spells you cast").
    assert_eq!(
        display_spell_cost(r.state(), P1, p1_looting),
        Some(cost(2, vec![ManaCostShard::Red])),
        "P1's Faithless Looting flashback {{2}}{{R}} is not reduced by P0's Bilbo"
    );

    assert_eq!(
        pool_size(&r, P0),
        0,
        "the pool funded exactly {{1}}{{R}} + {{2}}{{U}} and is now empty"
    );
    assert_settled(&r, P0, "Bilbo, Thief in the Night");
}

// ===== Sage of the Beyond: the reduction cannot cut coloured mana =====

/// Sage of the Beyond ({5}{U}{U} 5/5, Flying, Foretell {4}{U}): "Spells you cast
/// from anywhere other than your hand cost {2} less to cast." Dream Twist
/// ({U} Instant): "Target player mills three cards.\nFlashback {1}{U}".
///
/// CR 601.2f: the printed rulings say the reduction applies only to the generic
/// component of the total cost. The Dream Twist flashback {1}{U} has one generic
/// mana, so the {2} reduction is clamped at the {0} floor and the {U} shard
/// survives. Paired reach guard on the SAME card and route: with no Sage the
/// same graveyard flashback shows generic 1 + {U}, so the reduction is what
/// moved generic 1 to 0. Then the reduced cast is paid with exactly {U}.
#[test]
fn sage_reduction_never_touches_coloured_mana_dream_twist() {
    // With the Sage on the battlefield.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("U"));
    let _sage = scenario.add_real_card(P0, "Sage of the Beyond", Zone::Battlefield, db());
    let dream_twist = scenario.add_real_card(P0, "Dream Twist", Zone::Graveyard, db());
    scenario.with_library_top(P1, &["Forest"; 3]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    assert_eq!(
        display_spell_cost(r.state(), P0, dream_twist),
        Some(cost(0, vec![ManaCostShard::Blue])),
        "flashback {{1}}{{U}} floors at {{U}}: the {{2}} reduction cannot remove the blue shard"
    );
    let reduced = r.cast(dream_twist).target_player(P1).resolve();
    assert_eq!(
        reduced.zone_of(dream_twist),
        Zone::Exile,
        "the flashback cast resolves and then exiles the card"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the reduced flashback charged exactly {{U}}"
    );
    assert_eq!(
        r.state().players[P1.0 as usize].graveyard.len(),
        3,
        "the resolved Dream Twist milled P1's three cards"
    );
    assert_settled(&r, P0, "Dream Twist with Sage of the Beyond");

    // Reach guard: no Sage, same card, same graveyard route.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("CU"));
    let dream_twist = scenario.add_real_card(P0, "Dream Twist", Zone::Graveyard, db());
    scenario.with_library_top(P1, &["Forest"; 3]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    assert_eq!(
        display_spell_cost(r.state(), P0, dream_twist),
        Some(cost(1, vec![ManaCostShard::Blue])),
        "without the Sage the same flashback shows its full generic 1 + {{U}}"
    );
    let unreduced = r.cast(dream_twist).target_player(P1).resolve();
    assert_eq!(
        unreduced.zone_of(dream_twist),
        Zone::Exile,
        "the control flashback cast resolves and then exiles the card"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the control flashback charged exactly {{1}}{{U}}"
    );
    assert_settled(&r, P0, "Dream Twist control");
}

// ===== Sage of the Beyond: commander tax =====

/// Sage of the Beyond ({5}{U}{U} 5/5, Flying, Foretell {4}{U}): "Spells you
/// cast from anywhere other than your hand cost {2} less to cast." Niv-Mizzet,
/// Parun ({U}{U}{U}{R}{R}{R}) is P0's commander and has been cast from the
/// command zone once, so the command-zone origin carries the {2} commander tax.
///
/// CR 601.2f: the printed {2} reduction enters the total cost of the recast
/// from the command zone (CR 601.2a: the cast moves the commander from the
/// command zone onto the stack), so the tax {2} and the reduction {2} cancel
/// and the recast costs the six printed coloured mana exactly. Without the Sage
/// the same recast pays the printed cost plus the {2} tax. The control arm is
/// what pins the tax: the Sage arm alone would look the same if the tax were
/// never charged, since the printed generic component is already {0}.
#[test]
fn sage_of_the_beyond_reduces_a_commander_recast_after_tax() {
    let niv_shards = vec![
        ManaCostShard::Blue,
        ManaCostShard::Blue,
        ManaCostShard::Blue,
        ManaCostShard::Red,
        ManaCostShard::Red,
        ManaCostShard::Red,
    ];

    // With the battlefield Sage: tax {2} plus reduction {2} nets the printed cost.
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("UUURRR"));
    scenario.add_real_card(P0, "Sage of the Beyond", Zone::Battlefield, db());
    let niv = scenario.add_real_card(P0, "Niv-Mizzet, Parun", Zone::Hand, db());
    scenario.with_commander(niv);
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    // One prior cast from the command zone -> the {2} additional cost.
    r.state_mut().commander_cast_count.insert(niv, 1);
    assert_eq!(
        zone_of(&r, niv),
        Zone::Command,
        "reach guard: the commander sits in the command zone"
    );
    assert_eq!(
        engine::game::commander::commander_tax(r.state(), niv),
        2,
        "reach guard: one prior cast put the {{2}} commander tax on the recast"
    );
    assert_eq!(
        display_spell_cost(r.state(), P0, niv),
        Some(cost(0, niv_shards.clone())),
        "the taxed recast {{2}}{{U}}{{U}}{{U}}{{R}}{{R}}{{R}} is reduced back to the printed {{U}}{{U}}{{U}}{{R}}{{R}}{{R}} by the Sage"
    );
    let card_id = r.state().objects[&niv].card_id;
    r.act(GameAction::CastSpell {
        object_id: niv,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("the commander recast is payable with exactly the six printed coloured mana");
    assert_eq!(
        zone_of(&r, niv),
        Zone::Stack,
        "Niv-Mizzet was cast from the command zone onto the stack"
    );
    assert!(
        r.stack_names()
            .iter()
            .any(|name| name == "Niv-Mizzet, Parun"),
        "Niv-Mizzet, Parun is on the stack: {:?}",
        r.stack_names()
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the recast charged exactly {{U}}{{U}}{{U}}{{R}}{{R}}{{R}}"
    );
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completed without a further prompt: {:?}",
        r.state().waiting_for
    );

    // Control without the Sage: the same one-prior-cast recast pays the printed
    // cost plus the {2} tax.
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let niv = scenario.add_real_card(P0, "Niv-Mizzet, Parun", Zone::Hand, db());
    scenario.with_commander(niv);
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    r.state_mut().commander_cast_count.insert(niv, 1);
    assert_eq!(
        engine::game::commander::commander_tax(r.state(), niv),
        2,
        "reach guard: the control arm carries the same {{2}} commander tax"
    );
    assert_eq!(
        display_spell_cost(r.state(), P0, niv),
        Some(cost(2, niv_shards)),
        "without the Sage the same recast costs the printed cost plus the commander tax {{2}}"
    );
}

// ===== Fortune Teller's Talent =====

/// Fortune Teller's Talent ({U} Enchantment — Class), level 3 clause: "Spells
/// you cast from anywhere other than your hand cost {2} less to cast."
///
/// This witness covers the COST clause only, not card support: the level 1
/// look-at-top static and the level 2 top-of-library play permission are not
/// asserted here.
///
/// CR 601.2f: at level 2 the top-of-library cast (CR 601.2a: the card moves
/// from the library onto the stack) costs its full printed {2}{U}; only the
/// level 3 clause reduces it to {U}, while a hand cast stays full price at
/// both levels. The pre-fix parse (no origin filter) reduced the hand cast
/// too; at level 2 the level 3 static is inactive on either parse, so the
/// level-2 control below proves the reduction comes from level 3.
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
    // was cast this turn) but NOT reduced. At level 2 the level 3 static is
    // inactive on either parse, so this control cannot discriminate the two
    // parses — what it proves is that the {2} reduction comes from the
    // level 3 clause, not from the level 2 window.
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

    // Fund the Bears only after the earlier costs have consumed their pool,
    // so an earlier generic cost cannot spend the green mana.
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the earlier pool funded exactly {{3}}{{U}} + {{2}}{{U}} + {{2}}{{U}} + {{U}}"
    );
    for unit in units("CG") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    r.cast(drawn_bears_a).resolve();
    assert_eq!(zone_of(&r, drawn_bears_a), Zone::Battlefield);
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the hand cast charged exactly {{1}}{{G}} with level 3 active"
    );
    assert_settled(&r, P0, "Fortune Teller's Talent");
}

// ===== Fortune Teller's Talent: the level 2 top-of-library gate =====

/// Fortune Teller's Talent ({U} Enchantment — Class), level 2 clause: "As long
/// as you've cast a spell this turn, you may play cards from the top of your
/// library."
///
/// CR 601.2a + CR 601.2f: until a spell has been cast this turn, the level 2
/// window is closed, so the Divination on top of the library is neither shown
/// as castable nor accepted as a cast — the pool still holds the spare {2}{U},
/// so the cost is affordable and the rejection discriminates on the gate, not
/// on mana. Casting the hand Dream Twist ({U}; it targets P1, so P0's library
/// top is untouched) opens the window, and the top card then shows its full
/// printed {2}{U}; the level 3 {2} reduction is inactive at level 2. The level 1
/// look-at-top static and the level 2 permission beyond this gate are not
/// card-support claims.
#[test]
fn fortune_tellers_talent_level_2_top_cast_needs_a_spell_cast_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding: Level 2 {3}{U} + the hand Dream Twist {U} + a spare
    // {2}{U} so the top-of-library Divination is affordable when the gate
    // rejects it — the rejection must be the gate, not the mana.
    scenario.with_mana_pool(P0, units("CCCCCUUU"));
    let talent = scenario.add_real_card(P0, "Fortune Teller's Talent", Zone::Battlefield, db());
    let dream_twist = scenario.add_real_card(P0, "Dream Twist", Zone::Hand, db());
    // Library, top first: the Divination the level 2 window gates is the top
    // card throughout; Dream Twist targets P1, so P0's library never changes.
    let top_divination = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    scenario.with_library_top(P1, &["Forest"; 3]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
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

    // THE gate: no spell has been cast this turn, so the printed "As long as
    // you've cast a spell this turn" window is closed. This display check is
    // the discriminating assertion: it ignores mana affordability.
    assert_eq!(
        display_spell_cost(r.state(), P0, top_divination),
        None,
        "at level 2 with no spell cast this turn the top-of-library Divination is not castable"
    );

    // The closure is real, not a display omission: the public cast action is
    // rejected and the card stays on top of the library. The pool still holds
    // the spare {2}{U}, so the {2}{U} cost is payable — the rejection is the
    // level 2 gate, not affordability.
    let card_id = r.state().objects[&top_divination].card_id;
    assert!(
        r.act(GameAction::CastSpell {
            object_id: top_divination,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err(),
        "with no spell cast this turn the top-of-library Divination cannot be cast"
    );
    assert_eq!(
        zone_of(&r, top_divination),
        Zone::Library,
        "the rejected cast leaves the Divination in the library"
    );
    assert_eq!(
        r.state().players[P0.0 as usize].library.front(),
        Some(&top_divination),
        "the rejected cast leaves the Divination on top of the library"
    );

    // Casting a spell this turn (Dream Twist from hand, targeting P1) opens
    // the window without touching P0's library top.
    let dream_twist_cast = r.cast(dream_twist).target_player(P1).resolve();
    assert_eq!(
        dream_twist_cast.zone_of(dream_twist),
        Zone::Graveyard,
        "the hand Dream Twist resolves and goes to the graveyard"
    );
    assert_eq!(
        r.state().players[P0.0 as usize].library.front(),
        Some(&top_divination),
        "Dream Twist targeted P1, so the Divination is still P0's library top"
    );
    assert_eq!(
        display_spell_cost(r.state(), P0, top_divination),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "after a spell was cast this turn the top-of-library Divination becomes castable at its full {{2}}{{U}}"
    );

    assert_eq!(
        pool_size(&r, P0),
        3,
        "the spare {{2}}{{U}} is unspent: the rejected cast spent no mana, and the level 2 bar plus Dream Twist consumed {{3}}{{U}} + {{U}}"
    );
    assert_settled(&r, P0, "Fortune Teller's Talent level 2 gate");
}

// ===== Advanced Reconstruction =====

/// Which card the level-3 Advanced Reconstruction trigger exiled from the
/// post-mill graveyard pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AdvancedReconstructionExile {
    MilledDivination,
    PreExistingBears,
}

/// Advanced Reconstruction ({3}{R} Enchantment — Class). Level 1: "At the
/// beginning of your first main phase, mill a card, then exile a card from your
/// graveyard at random. You may play the exiled card this turn." Level 2:
/// "Whenever one or more cards leave your graveyard, this Class deals 2 damage
/// to each opponent." Level 3: "Spells you cast from anywhere other than your
/// hand cost {2} less to cast."
///
/// CR 601.2f: leveled to 3 through the real activation actions, the next P0
/// precombat main is reached through public turn actions
/// (`advance_to_next_p0_main`), where the level 1 trigger (CR 608.2c) mills the
/// top card of the library and THEN exiles a card from the graveyard at random.
/// A second real card (Grizzly Bears) already sits in the graveyard, so the
/// random pool holds two cards: the milled Divination and the pre-existing
/// Bears. Per seed this helper asserts the sound invariant: exactly one of the
/// two moved to exile and the other stayed in the graveyard, the exiled card
/// carries the this-turn play permission, and it is cast from exile (CR 601.2a)
/// at its reduced cost — {U} for Divination, {G} for Grizzly Bears, both the
/// printed cost minus {2} — while a Divination cast from the hand pays the full
/// {2}{U}; the level 2 trigger deals exactly 2 damage to P1 when the exiled card
/// leaves the graveyard. Returns which of the two the random pick took.
fn advanced_reconstruction_level_3_outcome(seed: u64) -> AdvancedReconstructionExile {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding for the two level bars, {1}{R} each.
    scenario.with_mana_pool(P0, units("RRCC"));
    let ar = scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db());
    let hand_divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    let p1_divination = scenario.add_real_card(P1, "Divination", Zone::Hand, db());
    // A real card already in the graveyard makes the trigger's pool hold two
    // cards, so "exile a card from your graveyard at random" cannot be
    // satisfied by mechanically exiling the milled card alone.
    let gy_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    // Library, top first: the draw step takes the Island, the level 1 trigger
    // mills the Divination (appended after the Bears), and the two casts draw
    // the rest.
    let draw_step_card = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let milled = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    let _exiled_cast_draw_a = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _exiled_cast_draw_b = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    let _hand_cast_draw_a = scenario.add_real_card(P0, "Plains", Zone::Library, db());
    let _hand_cast_draw_b = scenario.add_real_card(P0, "Swamp", Zone::Library, db());
    // Draw-step filler for the turn cycle the witness crosses (P1 must not deck
    // out on their own draw step).
    scenario.with_library_top(P1, &["Forest"; 2]);

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

    // The next P0 precombat main, reached through public turn actions. The draw
    // step takes the top card, then the level 1 trigger fires at the beginning
    // of the main phase; `advance_to_next_p0_main` returns only with the stack
    // empty, so the level 1 trigger and the level 2 damage trigger behind it
    // have already resolved.
    let trigger_turn = r.state().turn_number;
    advance_to_next_p0_main(&mut r, trigger_turn, "Advanced Reconstruction level 3 turn");
    assert!(
        r.state().turn_number > trigger_turn,
        "a full turn cycle was crossed through public actions"
    );
    assert_eq!(
        r.state().phase,
        Phase::PreCombatMain,
        "the turn reached its first main phase"
    );
    assert_eq!(
        zone_of(&r, draw_step_card),
        Zone::Hand,
        "the draw step took the top card"
    );
    // The pick is random over the two cards in P0's graveyard. The scenario's
    // seed (GameScenario::new -> seed 42) fixes the outcome, but this witness
    // does not depend on which one it is.
    let in_exile = [milled, gy_bears]
        .iter()
        .filter(|id| zone_of(&r, **id) == Zone::Exile)
        .count();
    let in_graveyard = [milled, gy_bears]
        .iter()
        .filter(|id| zone_of(&r, **id) == Zone::Graveyard)
        .count();
    assert_eq!(
        (in_exile, in_graveyard),
        (1, 1),
        "exactly one of the milled card and the pre-existing card moved to exile, the other stayed in the graveyard"
    );
    let exiled = if zone_of(&r, milled) == Zone::Exile {
        milled
    } else {
        gy_bears
    };
    let stayed = if exiled == milled { gy_bears } else { milled };
    assert!(
        r.state().objects[&exiled]
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
        "the randomly exiled card carries the this-turn play permission, got {:?}",
        r.state().objects[&exiled].casting_permissions
    );
    assert_eq!(
        r.life(P1),
        18,
        "level 2: the card leaving the graveyard deals exactly 2 damage to P1"
    );

    // Fund this turn's casts exactly for whichever card was exiled: the exiled
    // Divination {U} or Grizzly Bears {G}, plus the hand Divination {2}{U}.
    let (reduced_cost, reduced_pool, reduced_zone) = if exiled == milled {
        (
            cost(0, vec![ManaCostShard::Blue]),
            units("CCUU"),
            Zone::Graveyard,
        )
    } else {
        (
            cost(0, vec![ManaCostShard::Green]),
            units("CCUG"),
            Zone::Battlefield,
        )
    };
    for unit in reduced_pool {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, exiled),
        Some(reduced_cost),
        "the exiled card's printed cost is reduced by {{2}} at level 3"
    );
    let exiled_cast = r.cast(exiled).resolve();
    assert_eq!(
        exiled_cast.zone_of(exiled),
        reduced_zone,
        "the exiled card is castable for exactly its reduced cost"
    );
    assert_eq!(
        zone_of(&r, stayed),
        Zone::Graveyard,
        "the card the random pick did not take stays in the graveyard"
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
        "this turn's pool funded exactly the exiled card's reduced cost plus the hand Divination's {{2}}{{U}} and is now empty"
    );
    assert_eq!(r.life(P1), 18, "no further cards left the graveyard");
    assert_settled(&r, P0, "Advanced Reconstruction");

    if exiled == milled {
        AdvancedReconstructionExile::MilledDivination
    } else {
        AdvancedReconstructionExile::PreExistingBears
    }
}

/// The level-3 pool is EVERY card in the graveyard after the mill, and the pick
/// is random over that pool. A single run cannot prove randomness: an "always
/// exile the milled card" implementation passes whenever the seeded pick heads
/// that way. So the per-seed witness is swept over a fixed seed set, and the
/// test requires both outcomes — the pre-existing Grizzly Bears exiled for at
/// least one seed and the milled Divination for at least one.
#[test]
fn advanced_reconstruction_level_3_reduces_the_exiled_cast_and_level_2_deals_damage() {
    const SEEDS: [u64; 24] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
    ];
    let mut saw_milled = false;
    let mut saw_preexisting = false;
    for seed in SEEDS {
        match advanced_reconstruction_level_3_outcome(seed) {
            AdvancedReconstructionExile::MilledDivination => saw_milled = true,
            AdvancedReconstructionExile::PreExistingBears => saw_preexisting = true,
        }
    }
    assert!(
        saw_milled && saw_preexisting,
        "the seed set {:?} produced a single exile outcome (milled: {saw_milled}, pre-existing: {saw_preexisting}); a single outcome means the pick is not random over the post-mill graveyard",
        SEEDS
    );
}

// ===== Advanced Reconstruction: the level gates =====

/// Advanced Reconstruction ({3}{R} Enchantment — Class). Level 1: "At the
/// beginning of your first main phase, mill a card, then exile a card from
/// your graveyard at random. You may play the exiled card this turn."
///
/// CR 601.2f + CR 601.2a: the {2} reduction is the LEVEL 3 clause ("Spells
/// you cast from anywhere other than your hand cost {2} less to cast."), so
/// with the Class left at level 1, and again at level 2, the
/// milled-and-exiled Divination can be played this turn but costs its full
/// printed {2}{U} — the cast moves it from exile onto the stack. Both casts
/// are real and exactly funded (a mis-charge leaves the pool non-empty and
/// fails the test). At level 1 the level 2 damage trigger is also inactive;
/// at level 2 the milled card leaving the graveyard deals exactly 2 damage
/// to P1.
#[test]
fn advanced_reconstruction_levels_1_and_2_charge_the_full_exiled_cost() {
    // Level 1: no level bar activated.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ar = scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db());
    // Library, top first: the draw step takes the Island, the level 1
    // trigger mills and exiles the Divination (the only graveyard card, so
    // the random exile is deterministic), and the cast draws the two basics.
    let draw_step_card = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let milled = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    let _cast_draw_a = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _cast_draw_b = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    // Draw-step filler for the turn cycle the witness crosses.
    scenario.with_library_top(P1, &["Forest"; 2]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(1),
        "the printed Class enters the battlefield at level 1"
    );

    // The next P0 precombat main, reached through public turn actions; the
    // stack is empty on return, so the level 1 trigger has already resolved.
    let trigger_turn = r.state().turn_number;
    advance_to_next_p0_main(&mut r, trigger_turn, "Advanced Reconstruction level 1 turn");
    assert!(
        r.state().turn_number > trigger_turn,
        "a full turn cycle was crossed through public actions"
    );
    assert_eq!(
        r.state().phase,
        Phase::PreCombatMain,
        "the turn reached its first main phase"
    );
    assert_eq!(
        zone_of(&r, draw_step_card),
        Zone::Hand,
        "the draw step took the top card"
    );
    assert_eq!(
        zone_of(&r, milled),
        Zone::Exile,
        "the trigger milled the Divination and exiled it from the graveyard"
    );
    assert_eq!(
        r.life(P1),
        20,
        "level 1: the level 2 damage trigger is inactive, no cards-left-graveyard damage"
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
        "the exiled Divination carries the printed this-turn play permission, got {:?}",
        r.state().objects[&milled].casting_permissions
    );

    // The level 1 gate: full price, really cast, exactly funded.
    for unit in units("CCU") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, milled),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "at level 1 the exiled Divination costs its full printed {{2}}{{U}}"
    );
    let level_one_cast = r.cast(milled).resolve();
    assert_eq!(
        level_one_cast.zone_of(milled),
        Zone::Graveyard,
        "at level 1 the exiled Divination is castable at full price"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the level 1 cast charged exactly {{2}}{{U}}"
    );
    assert_settled(&r, P0, "Advanced Reconstruction level 1");

    // Level 2: the {1}{R} bar is paid through the real activation action,
    // and the same cast still costs the full {2}{U}.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("CR"));
    let ar = scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db());
    let draw_step_card = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let milled = scenario.add_real_card(P0, "Divination", Zone::Library, db());
    let _cast_draw_a = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _cast_draw_b = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    // Draw-step filler for the turn cycle the witness crosses.
    scenario.with_library_top(P1, &["Forest"; 2]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    let level_two_bar = level_bar_index(&r, ar, 2);
    r.activate(ar, level_two_bar).resolve();
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(2),
        "the {{1}}{{R}} bar set level 2"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the level bar charged exactly {{1}}{{R}}"
    );

    // The next P0 precombat main, reached through public turn actions; the
    // stack is empty on return, so both level 1 and level 2 triggers have
    // already resolved.
    let trigger_turn = r.state().turn_number;
    advance_to_next_p0_main(&mut r, trigger_turn, "Advanced Reconstruction level 2 turn");
    assert!(
        r.state().turn_number > trigger_turn,
        "a full turn cycle was crossed through public actions"
    );
    assert_eq!(
        r.state().phase,
        Phase::PreCombatMain,
        "the turn reached its first main phase"
    );
    assert_eq!(
        zone_of(&r, draw_step_card),
        Zone::Hand,
        "the draw step took the top card"
    );
    assert_eq!(
        zone_of(&r, milled),
        Zone::Exile,
        "the trigger milled the Divination and exiled it from the graveyard"
    );
    assert_eq!(
        r.life(P1),
        18,
        "level 2: the milled card leaving the graveyard deals exactly 2 damage to P1"
    );
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(2),
        "reach guard: the Class is at level 2 for the full-price assertion"
    );

    // The level 2 gate: still full price, really cast, exactly funded.
    for unit in units("CCU") {
        let _ = r.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        display_spell_cost(r.state(), P0, milled),
        Some(cost(2, vec![ManaCostShard::Blue])),
        "at level 2 the exiled Divination still costs its full printed {{2}}{{U}}: the {{2}} reduction is the level 3 clause"
    );
    let level_two_cast = r.cast(milled).resolve();
    assert_eq!(
        level_two_cast.zone_of(milled),
        Zone::Graveyard,
        "at level 2 the exiled Divination is castable at full price"
    );
    assert_eq!(
        pool_size(&r, P0),
        0,
        "the level 2 cast charged exactly {{2}}{{U}}"
    );
    assert_eq!(r.life(P1), 18, "no further cards left the graveyard");
    assert_settled(&r, P0, "Advanced Reconstruction level 2");
}

// ===== Savvy Trader: a cast from outside the game (Garth One-Eye) =====

/// Answer every prompt of an accepted copy cast: accept the offer, announce X,
/// pick the target, pay from the pool. `offered` is the copy the cast offer
/// must name; when a target is declared it must be one of the offered legal
/// targets. Every arm asserts the acting player, and any prompt this driver
/// does not expect panics with the state instead of silently ending the cast.
fn drive_copy_cast(r: &mut GameRunner, offered: ObjectId, x: u32, target: Option<TargetRef>) {
    for _ in 0..24 {
        let action = match &r.state().waiting_for {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(*player, P0, "P0 answers the copy-cast offer");
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CastOffer { player, kind } => {
                assert_eq!(*player, P0, "P0 receives the copy cast offer");
                match kind {
                    CastOfferKind::GraveyardPaidCast { hit_card, .. } => assert_eq!(
                        *hit_card, offered,
                        "the offer names the copy the witness is driving"
                    ),
                    other => panic!("the copy cast is not a paid cast offer: {other:?}"),
                }
                GameAction::GraveyardPaidCastChoice {
                    choice: CastChoice::Cast,
                }
            }
            WaitingFor::ChooseXValue { player, .. } => {
                assert_eq!(*player, P0, "P0 announces X");
                GameAction::ChooseX { value: x }
            }
            WaitingFor::TargetSelection {
                player,
                target_slots,
                ..
            } => {
                assert_eq!(*player, P0, "P0 chooses the copy's target");
                let wanted = target
                    .as_ref()
                    .expect("the copy cast needs a declared target");
                assert!(
                    target_slots
                        .iter()
                        .any(|slot| slot.legal_targets.contains(wanted)),
                    "the offered legal targets contain the declared target, got {target_slots:?}"
                );
                GameAction::ChooseTarget {
                    target: Some(wanted.clone()),
                }
            }
            WaitingFor::ManaPayment { player, .. } => {
                assert_eq!(*player, P0, "P0 pays for the copy cast");
                GameAction::PassPriority
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("the copy cast stopped at an unexpected prompt: {other:?}"),
        };
        r.act(action).expect("the copy cast proceeds");
    }
    panic!(
        "the copy cast did not finish within 24 prompts, stuck at {:?}",
        r.state().waiting_for
    );
}

/// P0's Garth One-Eye (with Savvy Trader when asked) against P1's Black Lotus,
/// a real Disenchant in P0's hand for the from-hand control, and exactly two
/// white mana units in P0's pool.
fn garth_disenchant_copy_scenario(
    with_savvy_trader: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, units("WW"));
    let garth = scenario.add_real_card(P0, "Garth One-Eye", Zone::Battlefield, db());
    if with_savvy_trader {
        // P0's graveyard is empty, so the enters trigger has no legal target
        // even if setup were to put it on the stack.
        scenario.add_real_card(P0, "Savvy Trader", Zone::Battlefield, db());
    }
    let lotus = scenario.add_real_card(P1, "Black Lotus", Zone::Battlefield, db());
    let hand_disenchant = scenario.add_real_card(P0, "Disenchant", Zone::Hand, db());
    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    (r, garth, lotus, hand_disenchant)
}

/// Savvy Trader ({3}{G}): "Spells you cast from anywhere other than your hand
/// cost {1} less to cast."
///
/// CR 707.13 + CR 400.11: Garth One-Eye creates a copy of a card named from its
/// closed domain, and outside the game is not a zone, so the staged copy's cast
/// is from outside the game — never from the exile zone it is staged in.
/// CR 601.2f: Savvy Trader's {1} reduction enters the total cost of that cast,
/// so the Disenchant copy's {1}{W} becomes {W} and a pool of exactly two white
/// units keeps one. The control scenario without the Trader pays the full
/// {1}{W} from the same pool. A real Disenchant cast from P0's hand in the same
/// scenario shape keeps its full {1}{W}: the static excludes the controller's
/// own hand, so only the outside-the-game cast is reduced.
#[test]
fn savvy_trader_reduces_garths_copy_cast_from_outside_the_game() {
    for (with_savvy_trader, remaining_after_payment) in [(true, 1), (false, 0)] {
        let label = if with_savvy_trader {
            "with Savvy Trader"
        } else {
            "control without Savvy Trader"
        };
        let (mut r, garth, lotus, _hand_disenchant) =
            garth_disenchant_copy_scenario(with_savvy_trader);
        garth_choose(&mut r, garth, "Disenchant");
        let copy = r
            .state()
            .last_created_token_ids
            .first()
            .copied()
            .expect("the choose-and-copy resolution created the copy");
        assert!(
            r.state().objects[&copy].created_outside_game,
            "{label}: CR 707.13 + CR 400.11: the copy is created outside the game"
        );
        assert_eq!(
            r.state().objects[&copy].name,
            "Disenchant",
            "{label}: the copy the offer names is the chosen Disenchant"
        );
        drive_copy_cast(&mut r, copy, 0, Some(TargetRef::Object(lotus)));
        assert!(
            r.stack_names().iter().any(|name| name == "Disenchant"),
            "{label}: the Disenchant copy is on the stack: {:?} / {:?}",
            r.stack_names(),
            r.state().waiting_for
        );
        assert_eq!(
            pool_size(&r, P0),
            remaining_after_payment,
            "{label}: the copy cast paid {{W}} from the {{W}}{{W}} pool when Savvy Trader reduces it, and the full {{1}}{{W}} without it"
        );
        pass_until_stack_empty(&mut r, label);
        assert_eq!(
            zone_of(&r, lotus),
            Zone::Graveyard,
            "{label}: the Disenchant copy destroyed the targeted artifact"
        );
        // Once the copy resolves, the game returns to P0 with an empty stack —
        // the cast permission is spent, not left pending.
        assert_settled(&r, P0, label);
    }

    // From-hand control: the same board with the Trader reduces nothing, so a
    // real Disenchant cast from P0's hand pays its full printed {1}{W}.
    let (r, _garth, _lotus, hand_disenchant) = garth_disenchant_copy_scenario(true);
    assert_eq!(
        display_spell_cost(r.state(), P0, hand_disenchant),
        Some(cost(1, vec![ManaCostShard::White])),
        "a Disenchant cast from P0's hand pays its full printed {{1}}{{W}}, not the reduced {{W}}"
    );
}

/// Savvy Trader ({3}{G}): "When this creature enters, exile target permanent
/// card from your graveyard. You may play that card for as long as it remains
/// exiled."
///
/// CR 305.1: "play" authorizes the land special action, so the exiled Forest —
/// played, never cast — must move onto the battlefield through the public
/// `GameAction::PlayLand`, and the permission is live because the card remains
/// exiled. The unchosen Grizzly Bears keeps the target prompt open (the engine
/// auto-commits a unique legal target).
#[test]
fn savvy_trader_plays_an_exiled_land_through_play_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exact funding for the Trader's {3}{G}; playing a land costs no mana.
    scenario.with_mana_pool(P0, units("CCCG"));
    let savvy = scenario.add_real_card(P0, "Savvy Trader", Zone::Hand, db());
    let gy_forest = scenario.add_real_card(P0, "Forest", Zone::Graveyard, db());
    // A second permanent card in the graveyard keeps the target prompt open.
    let gy_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());

    let card_id = r.state().objects[&savvy].card_id;
    r.act(GameAction::CastSpell {
        object_id: savvy,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast Savvy Trader from hand for its full printed {3}{G}");

    pass_until_trigger_target_prompt(&mut r, "Savvy Trader enters");
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(gy_forest)),
    })
    .expect("exile the Forest through the public target action");
    pass_until_stack_empty(&mut r, "Savvy Trader enters trigger");

    assert_eq!(
        zone_of(&r, savvy),
        Zone::Battlefield,
        "Savvy Trader resolved"
    );
    assert_eq!(
        zone_of(&r, gy_forest),
        Zone::Exile,
        "the trigger exiled the Forest"
    );
    assert_eq!(
        zone_of(&r, gy_bears),
        Zone::Graveyard,
        "the unchosen Bears stays in the graveyard"
    );
    assert!(
        r.state().objects[&gy_forest]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == P0
            )),
        "the exiled Forest carries the play-from-exile permission for P0, got {:?}",
        r.state().objects[&gy_forest].casting_permissions
    );

    let card_id = r.state().objects[&gy_forest].card_id;
    r.act(GameAction::PlayLand {
        object_id: gy_forest,
        card_id,
    })
    .expect("play the exiled Forest through the public land action");
    assert_eq!(
        zone_of(&r, gy_forest),
        Zone::Battlefield,
        "the exiled Forest was played as a land"
    );
    assert_eq!(
        zone_of(&r, gy_bears),
        Zone::Graveyard,
        "the land play disturbed nothing else in the graveyard"
    );
    assert_eq!(pool_size(&r, P0), 0, "playing a land costs no mana");
    assert_settled(&r, P0, "Savvy Trader land play");
}

/// Advanced Reconstruction ({3}{R} Enchantment — Class). Level 1: "At the
/// beginning of your first main phase, mill a card, then exile a card from
/// your graveyard at random. You may play the exiled card this turn."
///
/// CR 305.1: the exiled card is a Forest — a land, so the printed "play"
/// permission is exercised through the public `GameAction::PlayLand`, not the
/// spell-cast path. This witness makes no cost-reduction claim: the {2}
/// reduction is the level 3 clause, inactive while the Class stays at level 1.
#[test]
fn advanced_reconstruction_level_1_plays_the_exiled_land_through_play_land() {
    // Level 1: no level bar activated.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ar = scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db());
    // Library, top first: the draw step takes the Island, the level 1
    // trigger mills and exiles the Forest (the only graveyard card, so the
    // random exile is deterministic), and the Mountain stays on top.
    let draw_step_card = scenario.add_real_card(P0, "Island", Zone::Library, db());
    let milled = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    let _library_filler = scenario.add_real_card(P0, "Mountain", Zone::Library, db());
    // Draw-step filler for the turn cycle the witness crosses.
    scenario.with_library_top(P1, &["Forest"; 2]);

    let mut r = scenario.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    assert_eq!(
        r.state().objects[&ar].class_level,
        Some(1),
        "the printed Class enters the battlefield at level 1"
    );

    // The next P0 precombat main, reached through public turn actions; the
    // stack is empty on return, so the level 1 trigger has already resolved.
    let trigger_turn = r.state().turn_number;
    advance_to_next_p0_main(&mut r, trigger_turn, "Advanced Reconstruction level 1 turn");
    assert!(
        r.state().turn_number > trigger_turn,
        "a full turn cycle was crossed through public actions"
    );
    assert_eq!(
        r.state().phase,
        Phase::PreCombatMain,
        "the turn reached its first main phase"
    );
    assert_eq!(
        zone_of(&r, draw_step_card),
        Zone::Hand,
        "the draw step took the top card"
    );
    assert_eq!(
        zone_of(&r, milled),
        Zone::Exile,
        "the trigger milled the Forest and exiled it from the graveyard"
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
        "the exiled Forest carries the printed this-turn play permission, got {:?}",
        r.state().objects[&milled].casting_permissions
    );

    let card_id = r.state().objects[&milled].card_id;
    r.act(GameAction::PlayLand {
        object_id: milled,
        card_id,
    })
    .expect("play the exiled Forest through the public land action");
    assert_eq!(
        zone_of(&r, milled),
        Zone::Battlefield,
        "the exiled Forest was played as a land"
    );
    assert_settled(&r, P0, "Advanced Reconstruction land play");
}
