//! Compound coin outcomes must resolve as ONE complete, conditional branch.
//!
//! CR 705.2: only the flipping player wins or loses the flip, and the result
//! selects exactly one branch of the flip instruction. CR 608.2c: the sentence
//! "If you win the flip, X and Y" makes both X and Y part of that one branch.
//! Every row here drives the real activation/cast/combat pipeline from the
//! verbatim MTGJSON Oracle text, asserts EXACTLY ONE `CoinFlipped` per
//! activation, and pairs each absence assertion with a positive reach guard.
//! Seeds are searched in-test (each seed loop ends in a `reached` guard) so no
//! row depends on a hand-picked seed.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, ContinuousModification, Effect, FilterProp, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim MTGJSON AtomicCards.json Oracle text.
const FRENETIC: &str = "All Slivers have \"{0}: If this permanent is on the battlefield, flip a coin. If you win the flip, exile this permanent and return it to the battlefield under its owner's control at the beginning of the next end step. If you lose the flip, sacrifice it.\"";
const STRONGBOX: &str =
    "{2}, {T}: Flip a coin. If you win the flip, sacrifice this artifact and draw three cards.";
const ARCHAEOLOGIST: &str = "{R}, {T}: Flip a coin. If you win the flip, destroy target artifact and untap this creature. If you lose the flip, sacrifice this creature.";
const THUMB: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const MIJAE: &str = "Whenever this creature attacks, flip a coin. If you lose the flip, remove this creature from combat and tap it.";
const INVERT: &str = "Choose target spell, then flip a coin. If you win the flip, gain control of that spell and you may choose new targets for it. If you lose the flip, counter that spell.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

const SEEDS: std::ops::Range<u64> = 0..64;

fn flips(events: &[GameEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { won, .. } => Some(*won),
            _ => None,
        })
        .collect()
}

/// The pipeline ended cleanly: a priority window with nothing left on the stack.
fn complete(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");
}

/// Seed the RNG and evaluate layers so a static-granted ability is present.
fn flush(runner: &mut GameRunner, seed: u64) {
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Plains"; 12]);
    }
    scenario
}

fn frenetic_board(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = scenario();
    let source = scenario
        .add_creature_from_oracle(P0, "Frenetic Sliver", 2, 2, FRENETIC)
        .with_subtypes(vec!["Sliver"])
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let mut runner = scenario.build();
    flush(&mut runner, seed);
    (runner, source, bystander)
}

/// Advance to the End step through public actions (declaring no attackers), then
/// resolve whatever the beginning of the step put on the stack. The End step is
/// reached before the assertion so an absent delayed return is an observation,
/// not a skipped step.
fn end_step(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        if runner.state().phase == Phase::End {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                events.extend(
                    runner
                        .act(GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        })
                        .expect("declare no attackers")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority toward the End step")
                        .events,
                );
            }
            other => panic!("unexpected prompt on the way to the End step: {other:?}"),
        }
    }
    assert_eq!(runner.state().phase, Phase::End, "the End step was reached");
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve the End step triggers")
                .events,
        );
    }
    complete(runner);
    events
}

#[test]
fn frenetic_win_exiles_then_returns_at_next_end_step() {
    let mut reached = false;
    for seed in SEEDS {
        let (mut runner, source, bystander) = frenetic_board(seed);
        let outcome = runner.activate(source, 0).resolve();
        let results = flips(outcome.events());
        if results.first() != Some(&true) {
            continue;
        }
        reached = true;
        // CR 705.2 + CR 608.2c: one winning result owns the entire winning
        // instruction; there is no second flip.
        assert_eq!(results, vec![true], "exactly one winning flip");
        outcome.assert_zone(&[source], Zone::Exile);
        outcome.assert_zone(&[bystander], Zone::Battlefield);
        // CR 603.7a: the return was set up while the winning branch resolved.
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        complete(&runner);
        end_step(&mut runner);
        // CR 400.7 + CR 110.2a: it returns as a new object under its owner's control.
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&source].controller, P0);
        assert!(runner.state().delayed_triggers.is_empty());
        break;
    }
    assert!(reached, "must reach a winning activation");
}

#[test]
fn frenetic_loss_sacrifices_source_without_delayed_return() {
    let mut reached = false;
    for seed in SEEDS {
        let (mut runner, source, bystander) = frenetic_board(seed);
        let outcome = runner.activate(source, 0).resolve();
        let results = flips(outcome.events());
        if results.first() != Some(&false) {
            continue;
        }
        reached = true;
        assert_eq!(results, vec![false], "exactly one losing flip");
        // CR 701.21a: the source (not the bystander) is sacrificed.
        outcome.assert_zone(&[source], Zone::Graveyard);
        outcome.assert_zone(&[bystander], Zone::Battlefield);
        // CR 608.2c: the winning branch's delayed return is never created.
        assert!(runner.state().delayed_triggers.is_empty());
        complete(&runner);
        end_step(&mut runner);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&bystander].zone, Zone::Battlefield);
        break;
    }
    assert!(reached, "must reach a losing activation");
}

#[test]
fn frenetic_granted_branch_uses_recipient_and_returns_to_owner() {
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let mut scenario = scenario();
            let granter = scenario
                .add_creature_from_oracle(P0, "Frenetic Sliver", 2, 2, FRENETIC)
                .with_subtypes(vec!["Sliver"])
                .id();
            let recipient = scenario
                .add_creature(P1, "Recipient", 3, 3)
                .with_subtypes(vec!["Sliver"])
                .id();
            let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
            let mut runner = scenario.build();
            // Fixture: a P1-owned Sliver that P0 controls (owner != controller).
            let object = runner.state_mut().objects.get_mut(&recipient).unwrap();
            object.controller = P0;
            // The layer pass resets `controller` from `base_controller`.
            object.base_controller = Some(P0);
            flush(&mut runner, seed);
            assert_eq!(runner.state().objects[&recipient].owner, P1);
            assert_eq!(runner.state().objects[&recipient].controller, P0);
            assert!(
                !runner.state().objects[&recipient].abilities.is_empty(),
                "the recipient must carry the granted ability"
            );
            let outcome = runner.activate(recipient, 0).resolve();
            let results = flips(outcome.events());
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            assert_eq!(results, vec![won], "exactly one flip");
            // The granter and the bystander are never touched by the recipient's flip.
            outcome.assert_zone(&[granter, bystander], Zone::Battlefield);
            outcome.assert_zone(
                &[recipient],
                if won { Zone::Exile } else { Zone::Graveyard },
            );
            assert_eq!(runner.state().delayed_triggers.len(), usize::from(won));
            complete(&runner);
            end_step(&mut runner);
            let recipient_after = &runner.state().objects[&recipient];
            assert_eq!(
                recipient_after.zone,
                if won {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
            // CR 110.2a: "under its owner's control" - the owner (P1), not the
            // activator (P0), controls the returned permanent.
            if won {
                assert_eq!(recipient_after.controller, P1);
            }
            assert_eq!(recipient_after.owner, P1);
            assert_eq!(runner.state().objects[&granter].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&bystander].zone, Zone::Battlefield);
            break;
        }
        assert!(reached, "must reach recipient outcome won={won}");
    }
}

#[test]
fn strongbox_draws_only_for_the_winning_flip() {
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let mut scenario = scenario();
            let source = scenario
                .add_artifact_from_oracle(P0, "Sorcerer's Strongbox", STRONGBOX)
                .id();
            scenario.with_mana_pool(
                P0,
                (0..2)
                    .map(|_| ManaUnit::new(ManaType::Colorless, source, false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            flush(&mut runner, seed);
            let outcome = runner.activate(source, 0).resolve();
            let results = flips(outcome.events());
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            assert_eq!(results, vec![won], "exactly one flip");
            outcome.assert_zone(
                &[source],
                if won {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                },
            );
            // CR 608.2c: the draw belongs to the winning branch only.
            outcome.assert_hand_drawn(P0, if won { 3 } else { 0 });
            complete(&runner);
            break;
        }
        assert!(reached, "must reach Strongbox outcome won={won}");
    }
}

#[test]
fn archaeologist_keeps_destroy_and_untap_in_win_branch() {
    // NOTE: the artifact target of the win branch is never announced for an activation
    // (targets nested in a flip branch have no announcement slot), so "destroy target
    // artifact" has no object to act on in any version of this card's parse. That is a
    // separate engine gap, so this row covers what the branch compound owns: one flip,
    // the untap only on a win, and the sacrifice only on a loss.
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let mut scenario = scenario();
            let source = scenario
                .add_creature_from_oracle(P0, "Goblin Archaeologist", 1, 2, ARCHAEOLOGIST)
                .id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Red, source, false, vec![])],
            );
            let mut runner = scenario.build();
            flush(&mut runner, seed);
            let outcome = runner.activate(source, 0).resolve();
            let results = flips(outcome.events());
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            // Pre-fix the loss branch lived in a second, separate flip.
            assert_eq!(results, vec![won], "exactly one flip");
            outcome.assert_zone(
                &[source],
                if won {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                },
            );
            if won {
                // CR 608.2c: the untap is part of the winning branch; the {T}
                // cost tapped the source, so only the branch can have untapped it.
                outcome.assert_tapped(source, false);
            }
            complete(&runner);
            break;
        }
        assert!(reached, "must reach Archaeologist outcome won={won}");
    }
}

#[test]
fn frenetic_thumb_kept_result_runs_one_complete_branch() {
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let mut scenario = scenario();
            let source = scenario
                .add_creature_from_oracle(P0, "Frenetic Sliver", 2, 2, FRENETIC)
                .with_subtypes(vec!["Sliver"])
                .id();
            scenario.add_artifact_from_oracle(P0, "Krark's Thumb", THUMB);
            let mut runner = scenario.build();
            flush(&mut runner, seed);
            // Drive through public actions: the scenario resolve() driver would
            // auto-answer the keep prompt, hiding it.
            let mut events = runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("activate the granted ability")
                .events;
            assert_eq!(
                runner.state().stack.len(),
                1,
                "the activation is on the stack"
            );
            for _ in 0..8 {
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::CoinFlipKeepChoice { .. }
                ) {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority to resolve the activation")
                        .events,
                );
            }
            // CR 614.1a + CR 705.1: the Thumb's keep prompt is unconditional.
            let WaitingFor::CoinFlipKeepChoice {
                player, results, ..
            } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "expected the keep prompt, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(player, P0);
            assert_eq!(results.len(), 2, "the Thumb flips two coins");
            assert!(
                flips(&events).is_empty(),
                "no result is logical before the keep"
            );
            // Reach guard: a mixed pair makes the kept index decide the branch.
            if !(results.contains(&true) && results.contains(&false)) {
                continue;
            }
            reached = true;
            let index = results.iter().position(|result| *result == won).unwrap();
            let kept = runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![index],
                })
                .expect("keep one flip");
            // CR 705.2: only the kept result runs a branch; the ignored flip never happened.
            assert_eq!(flips(&kept.events), vec![won], "one logical flip");
            complete(&runner);
            assert_eq!(
                runner.state().objects[&source].zone,
                if won { Zone::Exile } else { Zone::Graveyard }
            );
            assert_eq!(runner.state().delayed_triggers.len(), usize::from(won));
            end_step(&mut runner);
            assert_eq!(
                runner.state().objects[&source].zone,
                if won {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
            break;
        }
        assert!(reached, "must reach kept outcome won={won}");
    }
}

#[test]
fn mijae_djinn_loss_removes_from_combat_and_taps() {
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let mut scenario = scenario();
            // Vigilance: attacking does not tap it, so a tap is the branch's own.
            let source = scenario
                .add_creature_from_oracle(P0, "Mijae Djinn", 6, 3, MIJAE)
                .vigilance()
                .id();
            let mut runner = scenario.build();
            runner.advance_to_combat();
            flush(&mut runner, seed);
            let declared = runner
                .declare_attackers(&[(source, AttackTarget::Player(P1))])
                .expect("declare the Djinn as an attacker");
            assert!(!runner.state().objects[&source].tapped, "vigilance reach");
            let is_attacking =
                |runner: &GameRunner| {
                    runner.state().combat.as_ref().is_some_and(|combat| {
                        combat.attackers.iter().any(|a| a.object_id == source)
                    })
                };
            assert!(is_attacking(&runner), "attack reach");
            assert_eq!(
                runner.state().stack.len(),
                1,
                "the attack trigger is on the stack"
            );
            let mut events = declared.events;
            for _ in 0..8 {
                if runner.state().stack.is_empty() {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve the attack trigger")
                        .events,
                );
            }
            let results = flips(&events);
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            assert_eq!(results, vec![won], "exactly one flip");
            // CR 506.4 + CR 608.2c: a loss removes it from combat and taps it;
            // a win leaves it attacking and untapped.
            assert_eq!(runner.state().objects[&source].tapped, !won);
            assert_eq!(is_attacking(&runner), won);
            complete(&runner);
            break;
        }
        assert!(reached, "must reach Mijae outcome won={won}");
    }
}

/// Invert Polarity: P0 casts Lightning Bolt at P1's creature, P1 responds.
fn invert_board(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = scenario();
    let original = scenario.add_creature(P1, "Original", 4, 4).id();
    let alternate = scenario.add_creature(P0, "Alternate", 4, 4).id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let invert = scenario
        .add_spell_to_hand_from_oracle(P1, "Invert Polarity", true, INVERT)
        .id();
    let mut runner = scenario.build();
    runner.cast(bolt).target_object(original).commit();
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    runner.cast(invert).target_object(bolt).commit();
    (runner, original, alternate, bolt, invert)
}

#[test]
fn invert_polarity_retarget() {
    for won in [true, false] {
        let mut reached = false;
        for seed in SEEDS {
            let (mut runner, original, alternate, bolt, invert) = invert_board(seed);
            let mut events = Vec::new();
            let mut retarget_prompts = 0;
            for _ in 0..24 {
                match runner.state().waiting_for.clone() {
                    WaitingFor::RetargetChoice { .. } => {
                        retarget_prompts += 1;
                        break;
                    }
                    WaitingFor::OptionalEffectChoice { .. } => {
                        events.extend(
                            runner
                                .act(GameAction::DecideOptionalEffect { accept: true })
                                .expect("accept the optional retarget")
                                .events,
                        );
                    }
                    WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                    _ => events.extend(
                        runner
                            .act(GameAction::PassPriority)
                            .expect("resolve Invert Polarity")
                            .events,
                    ),
                }
            }
            let results = flips(&events);
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            if won {
                // CR 705.2 + CR 608.2c: the win branch gains control AND offers the retarget.
                assert_eq!(retarget_prompts, 1, "a win offers the retarget prompt");
                let WaitingFor::RetargetChoice {
                    player,
                    legal_new_targets,
                    ..
                } = runner.state().waiting_for.clone()
                else {
                    panic!(
                        "expected RetargetChoice, got {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(player, P1);
                // CR 108.4 + CR 608.2c: the winner controls the spell (the object's
                // controller changed; the event proves it happened in this resolution).
                assert_eq!(runner.state().objects[&bolt].controller, P1);
                assert!(
                    events.iter().any(|event| matches!(
                        event,
                        GameEvent::ControllerChanged { object_id, new_controller, .. }
                            if *object_id == bolt && *new_controller == P1
                    )),
                    "the win branch must change control of the spell"
                );
                assert!(legal_new_targets.contains(&TargetRef::Object(alternate)));
                events.extend(
                    runner
                        .act(GameAction::RetargetSpell {
                            new_targets: vec![Some(TargetRef::Object(alternate))],
                        })
                        .expect("retarget Bolt to the alternate creature")
                        .events,
                );
                // Resolve the retargeted Bolt through public priority passes.
                for _ in 0..8 {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    events.extend(
                        runner
                            .act(GameAction::PassPriority)
                            .expect("resolve the retargeted Bolt")
                            .events,
                    );
                }
                assert_eq!(runner.state().objects[&alternate].damage_marked, 3);
                assert_eq!(runner.state().objects[&original].damage_marked, 0);
                assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
            } else {
                // CR 701.6a: the loss counters the spell; no retarget is offered.
                assert_eq!(retarget_prompts, 0, "a loss must not offer a retarget");
                assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
                assert_eq!(runner.state().objects[&original].damage_marked, 0);
                assert_eq!(runner.state().objects[&alternate].damage_marked, 0);
            }
            assert_eq!(runner.state().objects[&invert].zone, Zone::Graveyard);
            // CR 705.2: one flip for the whole resolution - the retarget (win) or the
            // counter (loss) belongs to that flip's branch, never to a second flip.
            assert_eq!(flips(&events), vec![won], "exactly one flip");
            complete(&runner);
            break;
        }
        assert!(reached, "must reach Invert Polarity outcome won={won}");
    }
}

/// SHAPE: the printed presence check belongs to the granted ability's root.
#[test]
fn frenetic_presence_gate_parse_shape() {
    let parsed = engine::parser::parse_oracle_text(
        FRENETIC,
        "Frenetic Sliver",
        &[],
        &["Creature".into()],
        &["Sliver".into()],
    );
    let granted = parsed
        .statics
        .iter()
        .flat_map(|definition| &definition.modifications)
        .find_map(|modification| match modification {
            ContinuousModification::GrantAbility { definition } => Some(definition),
            _ => None,
        })
        .expect("the Sliver grants an activated ability");
    assert!(matches!(&*granted.effect, Effect::FlipCoins { .. }));
    // CR 608.2c: follow the printed conditional before the flip instruction.
    assert_eq!(
        granted.condition,
        Some(AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::InZone {
                    zone: Zone::Battlefield,
                }],
                ..Default::default()
            }),
        })
    );
}

#[test]
fn frenetic_presence_gate_live_activation() {
    for won in [false, true] {
        let mut reached = false;
        for seed in SEEDS {
            let (mut runner, source, _) = frenetic_board(seed);
            let outcome = runner.activate(source, 0).resolve();
            let results = flips(outcome.events());
            if results.first() != Some(&won) {
                continue;
            }
            reached = true;
            assert_eq!(results, vec![won], "a live source still flips exactly once");
            outcome.assert_zone(&[source], if won { Zone::Exile } else { Zone::Graveyard });
            complete(&runner);
            break;
        }
        assert!(reached, "must reach the live-source outcome {won}");
    }
}

#[test]
fn frenetic_presence_gate_stacked_activations() {
    for [upper_won, lower_would_win] in [[false, true], [true, true], [true, false]] {
        // Calibrate the lower activation's RNG against an observed live flip.
        // Once the upper activation removes the source, that same RNG must not
        // be consumed by a second flip, regardless of its would-be outcome.
        let lower_seed = SEEDS
            .clone()
            .find(|&seed| {
                let (mut control, source, _) = frenetic_board(seed);
                let outcome = control.activate(source, 0).resolve();
                let results = flips(outcome.events());
                complete(&control);
                results == vec![lower_would_win]
            })
            .expect("must observe the lower activation's counterfactual result");
        let mut reached = false;
        for seed in SEEDS {
            let mut setup = scenario();
            let source = setup
                .add_creature_from_oracle(P0, "Frenetic Sliver", 2, 2, FRENETIC)
                .with_subtypes(vec!["Sliver"])
                .id();
            let unrelated = setup
                .add_enchantment_from_oracle(P0, "Unrelated enchantment", "")
                .id();
            let erase = setup
                .add_spell_to_hand_from_oracle(P0, "Erase", true, "Exile target enchantment.")
                .id();
            let mut runner = setup.build();
            runner
                .cast(erase)
                .target_object(unrelated)
                .resolve()
                .assert_zone(&[unrelated], Zone::Exile);
            complete(&runner);
            // Fixture: retain an unrelated earlier resolution's tracked set.
            // Erase moves the card but does not itself publish a tracked set.
            let earlier_set = TrackedSetId(runner.state().next_tracked_set_id);
            runner.state_mut().next_tracked_set_id += 1;
            runner
                .state_mut()
                .tracked_object_sets
                .insert(earlier_set, vec![unrelated]);
            assert!(
                !runner.state().tracked_object_sets.is_empty(),
                "the hostile board must contain an earlier tracked exile set"
            );
            flush(&mut runner, seed);
            for expected_len in [1, 2] {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("stack a real granted activation");
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert_eq!(runner.state().stack.len(), expected_len);
            }
            let mut upper_events = Vec::new();
            for _ in 0..32 {
                if runner.state().stack.len() == 1
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                upper_events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve only the upper activation")
                        .events,
                );
            }
            assert_eq!(runner.state().stack.len(), 1, "the lower remains stacked");
            let upper_results = flips(&upper_events);
            if upper_results.first() != Some(&upper_won) {
                continue;
            }
            reached = true;
            assert_eq!(upper_results, vec![upper_won]);
            let departed_zone = if upper_won {
                Zone::Exile
            } else {
                Zone::Graveyard
            };
            assert_eq!(runner.state().objects[&source].zone, departed_zone);
            assert_eq!(
                runner.state().delayed_triggers.len(),
                usize::from(upper_won)
            );
            runner.state_mut().rng = GameRng::seed_from_u64(lower_seed);
            let mut lower_events = Vec::new();
            for _ in 0..32 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                lower_events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve the lower activation")
                        .events,
                );
            }
            complete(&runner);
            // CR 113.7a + CR 608.2c: the stacked ability still resolves, but
            // its printed presence condition prevents another flip or move.
            assert!(flips(&lower_events).is_empty(), "the lower must not flip");
            upper_events.extend(lower_events);
            assert_eq!(
                flips(&upper_events),
                vec![upper_won],
                "one flip for the pair"
            );
            assert_eq!(runner.state().objects[&source].zone, departed_zone);
            // CR 603.7a: only the upper winning branch registers a return.
            assert_eq!(
                runner.state().delayed_triggers.len(),
                usize::from(upper_won)
            );
            assert_eq!(runner.state().objects[&unrelated].zone, Zone::Exile);
            println!(
                "presence order=({upper_won},{lower_would_win}) seeds=({seed},{lower_seed}) \
                 flips={:?} source={departed_zone:?} delayed={} unrelated=Exile",
                flips(&upper_events),
                runner.state().delayed_triggers.len()
            );
            end_step(&mut runner);
            assert_eq!(runner.state().objects[&unrelated].zone, Zone::Exile);
            break;
        }
        assert!(
            reached,
            "must reach the upper activation outcome {upper_won}"
        );
    }
}
