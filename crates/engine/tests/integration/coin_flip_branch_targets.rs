//! Coin-branch announcement regressions and P0 payment-completion evidence.
//! Synthetic fixtures deliberately omit Crooked Scales' repeat instruction.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, CoinFlipRun, Effect, EffectKind, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::Rng;

// Synthetic, targetless source-local punishment: isolates payment completion
// from the missing announcement-time targets being investigated by T.
const PAYMENT_PROBE: &str = "{T}: Flip a coin. If you lose the flip, destroy this artifact unless you pay {3}. You gain 2 life.";

fn pin_outcomes(runner: &mut GameRunner, outcomes: &[bool]) {
    let seed = (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("bounded seed search finds the requested outcome sequence");
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

fn mana(scenario: &mut GameScenario, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

fn flips(events: &[GameEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } => {
                assert_eq!(*player_id, P0);
                Some(*won)
            }
            _ => None,
        })
        .collect()
}

fn coin_terminals(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::FlipCoins, source_id, .. }
                if *source_id == source)
        })
        .count()
}

fn finish(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority to finish the announced ability")
                .events,
        );
    }
    panic!("announced ability did not finish within 20 priority passes");
}

fn assert_finished(runner: &GameRunner) {
    // CR 117.3b: the active player receives priority after resolution completes.
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

// P0: evidence only. Correct expectations intentionally expose PR-B defects;
// these tests must not bless a missing terminal marker or an omitted tail.
fn payment_completion_probe(pay: bool) {
    // SHAPE reach guard: a single supported coin, a SelfRef destroy carrying
    // the unless cost, and an independent life-gain successor outside the branch.
    let parsed = parse_oracle_text(
        PAYMENT_PROBE,
        "Synthetic Payment Probe",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    let Effect::FlipCoins {
        flips: run,
        win_effect: None,
        lose_effect: Some(lose),
        ..
    } = root.effect.as_ref()
    else {
        panic!("required single lost-flip branch: {root:#?}");
    };
    assert_eq!(*run, CoinFlipRun::single());
    assert!(
        matches!(
            lose.effect.as_ref(),
            Effect::Destroy {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "{lose:#?}"
    );
    let cost = AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![],
            generic: 3,
        },
    };
    let unless = lose
        .unless_pay
        .as_ref()
        .expect("supported unless-pay modifier");
    assert_eq!(unless.cost, cost);
    assert_eq!(unless.payer, TargetFilter::Controller);
    assert!(lose.sub_ability.is_none(), "tail is not loss-dependent");
    assert!(matches!(
        root.sub_ability
            .as_ref()
            .expect("independent tail")
            .effect
            .as_ref(),
        Effect::GainLife { .. }
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Synthetic Payment Probe", PAYMENT_PROBE)
        .id();
    mana(&mut scenario, 3);
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    let mana_before = pool(&runner);
    let outcome = runner.activate(source, 0).stop_at_optional().resolve();
    let mut events = outcome.events().to_vec();
    let WaitingFor::UnlessPayment {
        player,
        cost: live_cost,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "required P0 unless-payment prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*live_cost, cost);
    assert_eq!(
        flips(&events),
        vec![false],
        "forced loss reaches the payment"
    );
    assert_eq!(runner.life(P0), 20, "tail waits for payment completion");
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(pool(&runner), mana_before);
    events.extend(
        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("public unless-payment answer")
            .events,
    );
    finish(&mut runner, &mut events);

    let observed = (
        flips(&events),
        coin_terminals(&events, source),
        runner.state().objects[&source].zone,
        runner.life(P0),
        runner.state().resolution_stack.len(),
        runner.state().stack.len(),
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        pool(&runner),
    );
    eprintln!("P0 pay={pay}: (flips, terminal, source_zone, life, frames, stack, priority, mana_after)={observed:?}; mana_before={mana_before}; waiting={:?}", runner.state().waiting_for);
    // CR 118.12a + CR 608.2c: payment suppresses only the destroy;
    // the independent life-gain instruction still runs exactly once.
    assert_eq!(
        observed,
        (
            vec![false],
            1,
            if pay {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            22,
            0,
            0,
            true,
            if pay { 0 } else { 3 }
        ),
        "P0 payment completion (PR-B)"
    );
    assert_finished(&runner);
}

#[test]
fn p0_paid_loss_preserves_source_and_completes_coin_and_tail() {
    payment_completion_probe(true);
}

#[test]
fn p0_declined_loss_destroys_source_and_completes_coin_and_tail() {
    payment_completion_probe(false);
}

// Byte-identical to data/mtgjson/AtomicCards.json .data["Chaotic Strike"][0].text.
const CHAOTIC_STRIKE: &str = "Cast this spell only during combat after blockers are declared.\nFlip a coin. If you win the flip, target creature gets +1/+1 until end of turn.\nDraw a card.";
// Synthetic supported two-branch fixture, not the complete Crooked Scales card.
// The payment/repeat suffix is deliberately absent from this T witness.
const TWO_BRANCHES: &str = "{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature you control.";

fn chaotic_fixture(with_creatures: bool) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    // SHAPE eligibility: the live parser must retain both the targeted win and
    // the independent draw, with exactly one coin rather than two coin nodes.
    let parsed = parse_oracle_text(
        CHAOTIC_STRIKE,
        "Chaotic Strike",
        &[],
        &["Instant".into()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    let Effect::FlipCoins {
        flips: run,
        win_effect: Some(win),
        lose_effect: None,
        ..
    } = root.effect.as_ref()
    else {
        panic!("required Chaotic Strike coin shape: {root:#?}");
    };
    assert_eq!(*run, CoinFlipRun::single());
    assert!(matches!(win.effect.as_ref(), Effect::Pump { .. }));
    assert!(win.sub_ability.is_none());
    let draw = root
        .sub_ability
        .as_ref()
        .expect("independent draw successor");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
    assert!(draw.sub_ability.is_none());

    let mut scenario = GameScenario::new();
    // Both restrictions allow this actual post-blockers combat step.
    scenario.at_phase(Phase::EndCombat);
    let creatures = if with_creatures {
        vec![
            scenario.add_creature(P0, "Chosen Bear", 2, 2).id(),
            scenario.add_creature(P1, "Distractor Bear", 4, 4).id(),
        ]
    } else {
        vec![]
    };
    scenario.with_library_top(P0, &["Draw Witness", "Library Reserve"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaotic Strike", true, CHAOTIC_STRIKE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    (runner, spell, creatures)
}

fn castable(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(action,
        GameAction::CastSpell { object_id, .. } if *object_id == spell)
    })
}

#[test]
fn chaotic_strike_win_pumps_announced_target_and_draws() {
    let (mut runner, spell, creatures) = chaotic_fixture(true);
    let outcome = runner.cast(spell).target_object(creatures[0]).resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: forced win actually resolves"
    );
    outcome.assert_hand_drawn(P0, 1);
    // CR 608.2c: the won branch pumps the announced creature, not a distractor.
    assert_eq!(
        outcome.power_toughness(creatures[0]),
        (3, 3),
        "won branch must affect the announced target"
    );
    assert_eq!(outcome.power_toughness(creatures[1]), (4, 4));
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    assert_finished(&runner);
}

#[test]
fn chaotic_strike_announces_its_target_before_flipping() {
    let (mut runner, spell, creatures) = chaotic_fixture(true);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast reaches announcement")
        .events;
    // CR 601.2c: target choice is part of announcement, before the coin flips.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "Chaotic Strike must announce its win-branch target before resolution, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    for creature in &creatures {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    assert!(flips(&events).is_empty());
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creatures[0])),
            })
            .expect("announce chosen creature")
            .events,
    );
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay announced spell")
                .events,
        );
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::BecomesTarget { source_id, target, .. }
            if *source_id == spell && *target == TargetRef::Object(creatures[0]))));
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_finished(&runner);
}

#[test]
fn chaotic_strike_cannot_be_cast_without_a_legal_target() {
    let (mut valid, spell, creatures) = chaotic_fixture(true);
    assert!(
        castable(&valid, spell),
        "reach: legal timing, mana and a target admit the spell"
    );
    let outcome = valid.cast(spell).target_object(creatures[0]).resolve();
    assert_eq!(flips(outcome.events()), vec![true]);
    outcome.assert_hand_drawn(P0, 1);
    assert_finished(&valid);

    let (mut empty, spell, _) = chaotic_fixture(false);
    let offered = castable(&empty, spell);
    let card_id = empty.state().objects[&spell].card_id;
    let result = empty.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    // CR 601.2c: even a contingent win target must exist to cast the spell.
    assert!(
        !offered && result.is_err(),
        "missing win-branch target must refuse casting: offered={offered}, result={:?}",
        result.as_ref().map(|result| &result.waiting_for)
    );
    assert_eq!(empty.state().objects[&spell].zone, Zone::Hand);
    assert_eq!(pool(&empty), 2);
    assert_finished(&empty);
}

fn two_branch_fixture(
    own_pool: bool,
    opponent_pool: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    // SHAPE eligibility: both supported Destroy leaves belong to this one coin.
    let parsed = parse_oracle_text(
        TWO_BRANCHES,
        "Synthetic Two-Branch Scales",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    let Effect::FlipCoins {
        flips: run,
        win_effect: Some(win),
        lose_effect: Some(lose),
        ..
    } = root.effect.as_ref()
    else {
        panic!("required supported two-branch fixture: {root:#?}");
    };
    assert_eq!(*run, CoinFlipRun::single());
    for branch in [win, lose] {
        assert!(matches!(branch.effect.as_ref(), Effect::Destroy { .. }));
        assert!(branch.unless_pay.is_none());
        assert!(branch.sub_ability.is_none());
    }
    assert!(root.sub_ability.is_none());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Synthetic Two-Branch Scales", TWO_BRANCHES)
        .id();
    let own = if own_pool {
        vec![
            scenario.add_creature(P0, "Chosen Own", 2, 2).id(),
            scenario.add_creature(P0, "Own Distractor", 3, 3).id(),
        ]
    } else {
        vec![]
    };
    let opposing = if opponent_pool {
        vec![
            scenario.add_creature(P1, "Chosen Opponent", 4, 4).id(),
            scenario.add_creature(P1, "Opponent Distractor", 5, 5).id(),
        ]
    } else {
        vec![]
    };
    mana(&mut scenario, 4);
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    (runner, source, own, opposing)
}

#[test]
fn two_branch_activation_announces_both_targets_before_flipping() {
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation reaches announcement")
        .events;
    // CR 601.2c + CR 602.2b: both branches declare before commitment and RNG.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "both coin-branch targets must be announced on activation, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        target_slots.len(),
        2,
        "one independently declared occurrence per outcome"
    );
    for creature in &opposing {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
        assert!(!target_slots[1]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    for creature in &own {
        assert!(target_slots[1]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
        assert!(!target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    assert!(flips(&events).is_empty());
    assert_eq!(pool(&runner), 4);
    assert!(!runner.state().objects[&source].tapped);
    for target in [opposing[0], own[0]] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .expect("announce branch target")
                .events,
        );
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay announced activation")
                .events,
        );
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(pool(&runner), 0);
    assert!(runner.state().objects[&source].tapped);
    for chosen in [opposing[0], own[0]] {
        assert!(
            events.iter().any(|event| matches!(event,
            GameEvent::BecomesTarget { source_id, target, .. }
                if *source_id == source && *target == TargetRef::Object(chosen))),
            "both announced occurrences emit BecomesTarget"
        );
    }
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_finished(&runner);
}

fn refuses_missing_pool(own_pool: bool, opponent_pool: bool) {
    let (mut valid, source, own, opposing) = two_branch_fixture(true, true);
    assert!(
        can_activate_ability_now(valid.state(), P0, source, 0),
        "reach: both pools admit this activation"
    );
    let outcome = valid
        .activate(source, 0)
        .target_objects(&[opposing[0], own[0]])
        .resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: the same supported activation resolves"
    );
    assert_eq!(pool(&valid), 0);
    assert!(valid.state().objects[&source].tapped);
    assert_finished(&valid);

    let (mut missing, source, _, _) = two_branch_fixture(own_pool, opponent_pool);
    let offered = can_activate_ability_now(missing.state(), P0, source, 0);
    let result = missing.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    // CR 601.2c + CR 602.2b: neither mandatory target pool may be absent.
    assert!(!offered && result.is_err(), "missing branch pool must refuse before commitment: own_pool={own_pool}, opponent_pool={opponent_pool}, offered={offered}, result={:?}", result.as_ref().map(|result| &result.waiting_for));
    assert_eq!(pool(&missing), 4);
    assert!(!missing.state().objects[&source].tapped);
    assert_finished(&missing);
}

#[test]
fn two_branch_activation_requires_an_own_creature() {
    refuses_missing_pool(false, true);
}

#[test]
fn two_branch_activation_requires_an_opponents_creature() {
    refuses_missing_pool(true, false);
}

#[test]
fn two_branch_activation_requires_both_creature_pools() {
    refuses_missing_pool(false, false);
}

#[test]
fn two_branch_win_destroys_only_the_announced_opponents_creature() {
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    let outcome = runner
        .activate(source, 0)
        .target_objects(&[opposing[0], own[0]])
        .resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: forced win actually resolves"
    );
    // CR 608.2c: only the selected outcome destroys its independently bound target.
    assert_eq!(
        outcome.zone_of(opposing[0]),
        Zone::Graveyard,
        "won branch must destroy the announced opponent target"
    );
    for untouched in [own[0], own[1], opposing[1], source] {
        assert_eq!(outcome.zone_of(untouched), Zone::Battlefield);
    }
    assert_eq!(coin_terminals(outcome.events(), source), 1);
    assert_eq!(pool(&runner), 0);
    assert!(runner.state().objects[&source].tapped);
    assert_finished(&runner);
}

#[test]
fn coin_branch_stack_persistence_is_strict_and_current_bindings_execute() {
    use engine::types::resolution::ResolutionStateWire;
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation reaches announcement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    for target in [opposing[0], own[0]] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("announce branch target");
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay activation");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: bound ability is on stack"
    );
    let current =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone())).unwrap();
    let before = runner.state().clone();
    for version in 1..=10 {
        let mut malformed = current.clone();
        malformed["stack"][0]["kind"]["data"]["ability"]
            .as_object_mut()
            .expect("actual activated stack ability")
            .remove("coin_branches")
            .expect("reach: bound stack carrier");
        malformed["resolution_state_version"] = serde_json::json!(version);
        if version == 1 {
            malformed
                .as_object_mut()
                .unwrap()
                .remove("resolution_frames");
        }
        let error = serde_json::from_value::<ResolutionStateWire>(malformed)
            .expect_err("missing stack binding is never reconstructed")
            .to_string();
        let expected = if version <= 9 {
            "RESTORE_INCOMPATIBLE_COIN_BRANCH_BINDINGS:"
        } else {
            "INVALID_COIN_BRANCH_BINDINGS:"
        };
        assert!(error.contains(expected), "v{version}: {error}");
        assert_eq!(
            *runner.state(),
            before,
            "failed restore leaves live state unchanged"
        );
    }
    *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(current)
        .unwrap()
        .into_game_state();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&opposing[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&own[0]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&opposing[1]].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn text_substitution_reaches_both_live_coin_branch_filters() {
    use engine::types::mana::ManaColor;
    // Synthetic supported pump grammar; neither real pump card has a color restriction.
    const COIN: &str = "Flip a coin. If you win the flip, target nonred creature gets +1/+1 until end of turn. If you lose the flip, target nonred creature gets +2/+2 until end of turn.";
    const CHANGE: &str = "Change the text of target spell or permanent by replacing all instances of one color word with another. (For example, you may change \"nonblack creature\" to \"nongreen creature\" or \"black mana\" to \"green mana.\")";
    for won in [true, false] {
        for changed in [false, true] {
            for chosen_color in [ManaColor::Blue, ManaColor::Green] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let chosen = scenario
                    .add_creature(P0, "Chosen", 2, 2)
                    .with_color(vec![chosen_color])
                    .id();
                let distractor = scenario
                    .add_creature(P1, "Distractor", 4, 4)
                    .with_color(vec![ManaColor::Green])
                    .id();
                let spell = scenario
                    .add_spell_to_hand_from_oracle(P0, "Synthetic Coin Pump", true, COIN)
                    .id();
                let change = scenario
                    .add_spell_to_hand_from_oracle(P1, "Synthetic Word Change", true, CHANGE)
                    .id();
                scenario.with_mana_pool(
                    P1,
                    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
                );
                let mut runner = scenario.build();
                pin_outcomes(&mut runner, &[won]);
                let mut committed = runner
                    .cast(spell)
                    .target_objects(&[chosen, chosen])
                    .commit();
                let outcome = if changed {
                    committed
                        .act(GameAction::PassPriority)
                        .expect("P0 passes to responder");
                    committed
                        .cast(change)
                        .target_object(spell)
                        .choose_option("Red -> Blue")
                        .commit()
                        .resolve()
                } else {
                    committed.resolve()
                };
                let illegal = changed && chosen_color == ManaColor::Blue;
                assert_eq!(
                    flips(outcome.events()),
                    if illegal { vec![] } else { vec![won] },
                    "reach: legal branches flip exactly once; all-illegal branches do not flip"
                );
                let expected = if illegal {
                    2
                } else if won {
                    3
                } else {
                    4
                };
                let chosen_object = &runner.state().objects[&chosen];
                assert_eq!(
                    (
                        chosen_object.power.unwrap(),
                        chosen_object.toughness.unwrap()
                    ),
                    (expected, expected),
                    "won={won}, changed={changed}, color={chosen_color:?}"
                );
                let other = &runner.state().objects[&distractor];
                assert_eq!((other.power.unwrap(), other.toughness.unwrap()), (4, 4));
                assert_finished(&runner);
            }
        }
    }
}

#[test]
fn installed_coin_replacements_round_trip_and_execute_the_selected_bound_branch() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;

    for family in [ReplacementEvent::Draw, ReplacementEvent::Planeswalk] {
        // None is the non-coin substitute control; both coin outcomes are pinned.
        for outcome in [Some(true), Some(false), None] {
            let gain = |amount| Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            };
            let substitute = match outcome {
                Some(_) => {
                    let mut won = AbilityDefinition::new(AbilityKind::Spell, gain(3));
                    won.description = Some("Synthetic replacement: won".into());
                    let mut lost = AbilityDefinition::new(AbilityKind::Spell, gain(7));
                    lost.description = Some("Synthetic replacement: lost".into());
                    Effect::FlipCoins {
                        flips: CoinFlipRun::single(),
                        win_effect: Some(Box::new(won)),
                        lose_effect: Some(Box::new(lost)),
                        flipper: TargetFilter::Controller,
                    }
                }
                None => gain(11),
            };
            let install = match family {
                ReplacementEvent::Draw => Effect::CreateDrawReplacement {
                    replacement_effect: Box::new(substitute),
                },
                ReplacementEvent::Planeswalk => Effect::CreatePlaneswalkReplacement {
                    replacement_effect: Box::new(substitute),
                },
                _ => unreachable!(),
            };
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let top = scenario.add_card_to_library_top(P0, "Library control");
            let installer = scenario
                .add_spell_to_hand(P0, "Synthetic replacement installer", true)
                .with_ability(install)
                .id();
            let draw = scenario
                .add_spell_to_hand(P0, "Synthetic draw producer", true)
                .with_ability(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                })
                .id();
            let mut runner = scenario.build();
            if family == ReplacementEvent::Planeswalk {
                let state = runner.state_mut();
                state.format_config = engine::types::format::FormatConfig::planechase();
                let plane = engine::game::zones::create_object(
                    state,
                    CardId(90_032),
                    P0,
                    "Synthetic active plane".into(),
                    Zone::Command,
                );
                state.objects.get_mut(&plane).unwrap().card_types.core_types =
                    vec![CoreType::Plane];
                state.command_zone.push_back(plane);
                state.planar_controller = Some(P0);
            }
            runner.cast(installer).resolve();
            assert_finished(&runner);
            assert_eq!(
                runner.state().pending_damage_replacements.len(),
                1,
                "reach: installed shield"
            );
            let runtime = runner.state().pending_damage_replacements[0]
                .runtime_execute
                .as_ref()
                .unwrap();
            assert_eq!(
                runtime.coin_branches.is_some(),
                outcome.is_some(),
                "reach: constructor materialized both outcomes"
            );
            let wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state();
            let life = runner.state().players[0].life;
            let mut events = Vec::new();
            if family == ReplacementEvent::Draw {
                pin_outcomes(&mut runner, &[outcome.unwrap_or(true)]);
                events.extend_from_slice(runner.cast(draw).resolve().events());
                assert_eq!(
                    runner.state().objects[&top].zone,
                    Zone::Library,
                    "replaced draw did not draw"
                );
            } else {
                let seed = (0..100_000)
                    .find(|&seed| {
                        let mut rng = GameRng::seed_from_u64(seed);
                        rng.draw(RandomDraw::Outcome).random_range(1..=6) == 1
                            && outcome.is_none_or(|won| {
                                rng.draw(RandomDraw::Outcome).random_bool(0.5) == won
                            })
                    })
                    .expect("seed pins planar symbol followed by coin outcome");
                let state = runner.state_mut();
                state.rng_seed = seed;
                state.rng_word_pos = 0;
                state.rng = GameRng::seed_from_u64(seed);
                events.extend(
                    runner
                        .act(GameAction::RollPlanarDie)
                        .expect("roll planar die through production action")
                        .events,
                );
                assert!(
                    events.iter().any(|event| matches!(
                        event,
                        GameEvent::PlanarDieRolled {
                            face: engine::game::planechase::PlanarDieFace::Planeswalk,
                            ..
                        }
                    )),
                    "reach: actual planeswalking roll"
                );
                finish(&mut runner, &mut events);
            }
            assert_eq!(
                flips(&events),
                outcome.into_iter().collect::<Vec<_>>(),
                "exact coin event, absent for non-coin control"
            );
            assert_eq!(
                runner.state().players[0].life,
                life + match outcome {
                    Some(true) => 3,
                    Some(false) => 7,
                    None => 11,
                },
                "{family:?}: selected substitute payoff"
            );
            assert_finished(&runner);
        }
    }
}

#[test]
fn delayed_parent_referent_never_reads_an_unselected_coin_siblings_declaration() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, QuantityExpr, TypedFilter,
    };
    use engine::types::counter::CounterType;

    for has_ancestor in [false, true] {
        let counter = |target| Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        };
        let creature = TargetFilter::Typed(TypedFilter::creature());
        let coin = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CreateDelayedTrigger {
                        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                        effect: Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            counter(TargetFilter::ParentTarget),
                        )),
                        uses_tracked_set: false,
                    },
                ))),
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    counter(creature.clone()),
                ))),
                flipper: TargetFilter::Controller,
            },
        );
        let definition = if has_ancestor {
            AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target: creature })
                .sub_ability(coin)
        } else {
            coin
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let ancestor = scenario
            .add_creature(P0, "Earlier chosen creature", 2, 2)
            .id();
        let sibling = scenario
            .add_creature(P1, "Unselected sibling target", 2, 2)
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic lexical coin probe", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        let targets = if has_ancestor {
            vec![ancestor, sibling]
        } else {
            vec![sibling]
        };
        let outcome = runner.cast(spell).target_objects(&targets).resolve();
        assert_eq!(
            flips(outcome.events()),
            vec![true],
            "reach: win creates delayed instruction, loss target remains unselected"
        );
        assert_finished(&runner);
        runner.advance_to_end_step();
        assert_eq!(
            runner.state().phase,
            Phase::End,
            "reach: actual next end step"
        );
        finish(&mut runner, &mut Vec::new());
        // CR 608.2c: the delayed clause's anaphor reads its antecedent, not an
        // object announced solely for the other random outcome.
        assert_eq!(
            runner.state().objects[&sibling]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
        assert_eq!(
            runner.state().objects[&ancestor]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            u32::from(has_ancestor),
            "positive control: genuine incoming antecedent remains bound"
        );
        assert_finished(&runner);
    }
}

fn pass_to_coin_keep(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "coin reaches its keep prompt"
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { player: P0, .. }
        ),
        "required keep prompt, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn thumb_restore_keeps_each_declared_branch_occurrence_and_refuses_pin_drift() {
    use engine::types::ability::{AbilityDefinition, AbilityKind};
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::resolution::ResolutionStateWire;

    for won in [false, true] {
        for stale in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_artifact_from_oracle(
                P0,
                "Krark's Thumb",
                "If you would flip a coin, instead flip two coins and ignore one.",
            );
            let own = scenario
                .add_creature(P0, "Own announced creature", 2, 2)
                .id();
            let opposing = scenario
                .add_creature(P1, "Opponent announced creature", 3, 3)
                .id();
            let distractor = scenario.add_creature(P1, "Never announced", 4, 4).id();
            let parsed = parse_oracle_text(
                TWO_BRANCHES,
                "Synthetic Scales",
                &[],
                &["Artifact".into()],
                &[],
            );
            let definition: AbilityDefinition = parsed.abilities[0].clone();
            assert_eq!(definition.kind, AbilityKind::Activated);
            let source = scenario
                .add_artifact_from_oracle(P0, "Synthetic Scales", TWO_BRANCHES)
                .id();
            mana(&mut scenario, 4);
            let mut runner = scenario.build();
            pin_outcomes(&mut runner, &[true, false]);
            let mut events = runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap()
                .events;
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ));
            for target in [opposing, own] {
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(target)),
                        })
                        .unwrap()
                        .events,
                );
            }
            if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            pass_to_coin_keep(&mut runner, &mut events);
            assert!(flips(&events).is_empty(), "ignored flips have not happened");
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { player: P0, .. }
            ));
            let pending = runner.state().active_coin_flip_frame().unwrap();
            for (branch, target) in [
                (pending.win_effect.as_deref().unwrap(), opposing),
                (pending.lose_effect.as_deref().unwrap(), own),
            ] {
                assert_eq!(branch.controller, P0);
                assert_eq!(branch.targets, vec![TargetRef::Object(target)]);
                assert_eq!(
                    branch.target_pin_at(0),
                    Some(ObjectIncarnationRef::from_object(
                        &runner.state().objects[&target]
                    ))
                );
            }
            runner.state_mut().capture_rng_word_pos();
            let wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            let mut malformed = wire.clone();
            let frame = malformed["resolution_frames"]["frames"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|frame| frame["type"] == "CoinFlip")
                .unwrap();
            let branch = &mut frame["data"]["bound_branches"]["won"];
            branch["target_pins"][0]["object_id"] = serde_json::to_value(distractor).unwrap();
            let error = serde_json::from_value::<ResolutionStateWire>(malformed)
                .unwrap_err()
                .to_string();
            assert!(error.contains("target pin"), "{error}");
            *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state();
            let selected = if won { opposing } else { own };
            if stale {
                // Saved-board hostile fixture: same storage id, different object identity.
                runner
                    .state_mut()
                    .objects
                    .get_mut(&selected)
                    .unwrap()
                    .incarnation += 1;
            }
            events.extend(
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![usize::from(!won)],
                    })
                    .unwrap()
                    .events,
            );
            finish(&mut runner, &mut events);
            assert_eq!(flips(&events), vec![won]);
            assert_eq!(
                runner.state().objects[&selected].zone,
                if stale {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
            assert_eq!(
                runner.state().objects[&if won { own } else { opposing }].zone,
                Zone::Battlefield
            );
            assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
            assert_finished(&runner);
        }
    }
}

#[test]
fn malformed_coin_carrier_refuses_before_rng_or_mutation_and_canonical_executes() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::flip_coin;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, EffectError, QuantityExpr, ResolvedAbility,
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Synthetic source", 2, 2).id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true, false, true]);
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    let malformed = ResolvedAbility::new((*definition.effect).clone(), vec![], source, P0);
    let before = serde_json::to_value(runner.state()).unwrap();
    let mut expected_rng = runner.state().rng.clone();
    let mut events = Vec::new();
    assert_eq!(
        flip_coin::resolve(runner.state_mut(), &malformed, &mut events),
        Err(EffectError::MissingParam("resolved coin branches".into()))
    );
    assert!(events.is_empty());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    let mut actual_rng = runner.state().rng.clone();
    let expected: Vec<bool> = (0..3)
        .map(|_| expected_rng.draw(RandomDraw::Outcome).random_bool(0.5))
        .collect();
    let actual: Vec<bool> = (0..3)
        .map(|_| actual_rng.draw(RandomDraw::Outcome).random_bool(0.5))
        .collect();
    assert_eq!(actual, expected, "rejection consumes no outcome draw");
    let canonical = build_resolved_from_def(&definition, source, P0);
    flip_coin::resolve(runner.state_mut(), &canonical, &mut events).unwrap();
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.life(P0), 23);
    assert_finished(&runner);
}

#[test]
fn bangchuckers_shaped_loss_never_borrows_the_win_only_target() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DamageSource, QuantityExpr, TypedFilter,
    };
    for (won, self_damage) in [(true, false), (false, false), (false, true)] {
        let damage = |target, damage_source| Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target,
            damage_source,
            excess: None,
        };
        let coin = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                damage(TargetFilter::Typed(TypedFilter::creature()), None),
            ))),
            lose_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                damage(
                    if self_damage {
                        TargetFilter::SelfRef
                    } else {
                        TargetFilter::ParentTarget
                    },
                    if self_damage {
                        None
                    } else {
                        Some(DamageSource::Target)
                    },
                ),
            ))),
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Synthetic Bangchucker", 3, 5)
            .with_ability_definition(AbilityDefinition::new(AbilityKind::Activated, coin))
            .id();
        let target = scenario.add_creature(P1, "Win-only target", 4, 5).id();
        let distractor = scenario.add_creature(P1, "Distractor", 4, 5).id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        let outcome = runner.activate(source, 0).target_object(target).resolve();
        assert_eq!(flips(outcome.events()), vec![won]);
        let dealt: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target,
                    amount,
                    ..
                } => Some((*source_id, target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dealt,
            if won {
                vec![(source, TargetRef::Object(target), 2)]
            } else if self_damage {
                vec![(source, TargetRef::Object(source), 2)]
            } else {
                vec![]
            }
        );
        assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn repeated_bound_branch_recomputes_counter_quantity_from_a_pristine_template() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ObjectScope, QuantityExpr, QuantityRef, TypedFilter,
    };
    use engine::types::counter::CounterType;
    for until_loss in [false, true] {
        let coin = Effect::FlipCoins {
            flips: if until_loss {
                CoinFlipRun::UntilLoss
            } else {
                CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 3 },
                }
            },
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::Target,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "Repeated recipient", 2, 2).id();
        scenario.with_counter(target, CounterType::Plus1Plus1, 2);
        let distractor = scenario.add_creature(P1, "Unchanged recipient", 7, 7).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic repeated growing reward", true)
            .with_ability(coin)
            .id();
        let mut runner = scenario.build();
        let expected_flips = if until_loss {
            vec![true, true, true, false]
        } else {
            vec![true, true, true]
        };
        pin_outcomes(&mut runner, &expected_flips);
        let outcome = runner.cast(spell).target_object(target).resolve();
        assert_eq!(flips(outcome.events()), expected_flips);
        let counters: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::CounterAdded {
                    object_id,
                    counter_type: CounterType::Plus1Plus1,
                    count,
                    ..
                } => {
                    assert_eq!(*object_id, target);
                    Some(*count)
                }
                _ => None,
            })
            .collect();
        assert_eq!(counters, vec![2, 4, 8]);
        #[derive(Debug, PartialEq, Eq)]
        enum RunEvent {
            Flip(bool),
            Counter(u32),
            Complete,
        }
        use RunEvent::{Complete, Counter, Flip};
        let stream: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { won, .. } => Some(Flip(*won)),
                GameEvent::CounterAdded {
                    object_id, count, ..
                } if *object_id == target => Some(Counter(*count)),
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoins,
                    source_id,
                    ..
                } if *source_id == spell => Some(Complete),
                _ => None,
            })
            .collect();
        assert_eq!(
            stream,
            if until_loss {
                vec![
                    Flip(true),
                    Flip(true),
                    Flip(true),
                    Flip(false),
                    Counter(2),
                    Counter(4),
                    Counter(8),
                    Complete,
                ]
            } else {
                vec![
                    Flip(true),
                    Counter(2),
                    Flip(true),
                    Counter(4),
                    Flip(true),
                    Counter(8),
                    Complete,
                ]
            }
        );
        assert_eq!(
            runner.state().objects[&target].counters[&CounterType::Plus1Plus1],
            16
        );
        assert_eq!(runner.state().objects[&distractor].power, Some(7));
        assert_eq!(coin_terminals(outcome.events(), spell), 1);
        assert_finished(&runner);
    }
}

#[test]
fn independent_coin_declaration_owns_its_pins_even_when_incoming_id_is_reused() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    use engine::types::counter::CounterType;
    use engine::types::identifiers::ObjectIncarnationRef;
    for legacy_keyed_child in [false, true] {
        let coin = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "New incarnation", 2, 2).id();
        let distractor = scenario.add_creature(P1, "Unchosen", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic independently targeted coin", true)
            .with_ability(coin)
            .id();
        let mut runner = scenario.build();
        let old = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        runner
            .state_mut()
            .objects
            .get_mut(&target)
            .unwrap()
            .incarnation += 1;
        pin_outcomes(&mut runner, &[true]);
        drop(runner.cast(spell).target_object(target).commit());
        let current = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        let root = runner
            .state_mut()
            .stack
            .back_mut()
            .unwrap()
            .ability_mut()
            .unwrap();
        root.set_target_incarnations_recursive(vec![old]);
        let child = root.coin_branches.as_mut().unwrap().won.as_mut().unwrap();
        assert!(
            child.target_incarnations.is_empty(),
            "incoming delayed-reference pins never stamp an independent declaration"
        );
        assert_eq!(child.target_pin_at(0), Some(current));
        if legacy_keyed_child {
            // A pre-existing keyed reference cannot veto a fresh positional occurrence.
            child.target_incarnations = vec![old];
        }
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true]);
        assert_eq!(
            runner.state().objects[&target].counters[&CounterType::Plus1Plus1],
            3
        );
        assert!(!runner.state().objects[&distractor]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        assert_finished(&runner);
    }
}

#[test]
fn thumb_completed_empty_choice_restores_and_runs_only_independent_instructions() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, SubAbilityLink,
        TargetChoiceTiming, TypedFilter,
    };
    use engine::types::resolution::ResolutionStateWire;
    for populated in [false, true] {
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let loss = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::SpecificPlayer { id: P1 },
            },
        )
        .sub_ability(life);
        let coin = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(loss)),
                flipper: TargetFilter::Controller,
            },
        );
        let mut choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        )
        .sub_ability(coin);
        choice.target_choice_timing = TargetChoiceTiming::Resolution;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
        let picked =
            populated.then(|| scenario.add_creature(P0, "Only eligible choice", 2, 2).id());
        let distractor = scenario.add_creature(P1, "Ineligible choice", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic completed choice", true)
            .with_ability_definition(choice)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false, true]);
        drop(runner.cast(spell).commit());
        let mut events = Vec::new();
        pass_to_coin_keep(&mut runner, &mut events);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { player: P0, .. }
        ));
        let context = runner
            .state()
            .active_coin_flip_frame()
            .unwrap()
            .branch_context
            .clone();
        if populated {
            assert_eq!(
                context.parent_occurrences.targets,
                vec![TargetRef::Object(picked.unwrap())]
            );
            assert!(context.parent_occurrences.pins[0].is_some());
            assert!(context.forwarded_result_context.is_none());
        } else {
            assert!(context.parent_occurrences.targets.is_empty());
            assert!(context
                .forwarded_result_context
                .as_deref()
                .unwrap()
                .targets
                .is_empty());
        }
        runner.state_mut().capture_rng_word_pos();
        let wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap();
        *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
            .unwrap()
            .into_game_state();
        assert_eq!(
            runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context,
            context
        );
        events.extend(
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![0],
                })
                .unwrap()
                .events,
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![false]);
        assert_eq!(
            runner.life(P0),
            25,
            "independent instruction runs after the reference"
        );
        if let Some(picked) = picked {
            assert_eq!(runner.state().objects[&picked].controller, P1);
            assert!(events.iter().any(|event| matches!(event,
                GameEvent::ControllerChanged { object_id, .. } if *object_id == picked)));
        } else {
            assert!(!events
                .iter()
                .any(|event| matches!(event, GameEvent::ControllerChanged { .. })));
        }
        assert_eq!(runner.state().objects[&distractor].controller, P1);
        assert_finished(&runner);
    }
}
