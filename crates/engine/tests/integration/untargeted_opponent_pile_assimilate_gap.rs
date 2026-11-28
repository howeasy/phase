//! `ChangeZoneAll:untargeted_opponent_pile_assimilate` on the printed faces.
//!
//! CR 115.1d + CR 607.2a: Assimilation Assessor's entry clause exiles "target
//! opponent's graveyard" — a target chosen as the ability is put on the stack —
//! and its attack clause reads that pile ("assimilate a creature card exiled
//! with it"). The lowering carries the chosen opponent only as an
//! opponent-scoped `Typed` filter with no player slot, so resolution widens the
//! move to every opponent's graveyard in a multiplayer game, and the CR 607.2a
//! linked read then ranges over cards from graveyards the ability never chose.
//! The face is reported unsupported until the mass move declares the chosen
//! player. The label is keyed to the pair of shapes: Release to Memory (the same
//! opponent graveyard mass exile, no assimilate reader) stays gap-free for this,
//! and the assimilate-only faces (Borg Cube, Mimic Vat, Mysterious Limousine,
//! Duplicity, Moonring Mirror, Mirror of Life Trapping) never carry it.

use engine::database::CardDatabase;
use engine::game::coverage::{
    card_face_coverage, card_face_gaps, UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP,
};
use engine::types::ability::{AbilityDefinition, ControllerRef, Effect, TargetFilter, TypedFilter};
use engine::types::card::CardFace;
use engine::types::zones::Zone;

fn face(name: &str) -> CardFace {
    CardDatabase::from_integration_fixture_cached()
        .expect("integration fixture loads")
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the integration fixture"))
        .clone()
}

fn chain_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    out.push(&def.effect);
    if let Some(sub) = &def.sub_ability {
        chain_effects(sub, out);
    }
}

/// Every effect reachable through the face's abilities and trigger bodies by sub-ability chain.
fn face_effects(face: &CardFace) -> Vec<&Effect> {
    let mut out = Vec::new();
    for def in face
        .abilities
        .iter()
        .chain(face.triggers.iter().filter_map(|t| t.execute.as_deref()))
    {
        chain_effects(def, &mut out);
    }
    out
}

/// The "exile target opponent's graveyard" mass move lowered without a player slot.
fn is_opponent_graveyard_mass_exile(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZoneAll {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target:
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    type_filters,
                    ..
                }),
            ..
        } if type_filters.is_empty()
    )
}

/// The "assimilate a creature card exiled with it" linked-pile read.
fn is_assimilate_pile_read(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target,
            enters_under: Some(ControllerRef::You),
            enter_with_counters,
            ..
        } if !enter_with_counters.is_empty() && target.references_exiled_by_source()
    )
}

fn has_opponent_graveyard_mass_exile(face: &CardFace) -> bool {
    face_effects(face)
        .into_iter()
        .any(is_opponent_graveyard_mass_exile)
}

fn has_assimilate_pile_read(face: &CardFace) -> bool {
    face_effects(face).into_iter().any(is_assimilate_pile_read)
}

fn definition_has_assimilate_read(def: &AbilityDefinition) -> bool {
    let mut effects = Vec::new();
    chain_effects(def, &mut effects);
    effects.into_iter().any(is_assimilate_pile_read)
}

fn carries_gap(face: &CardFace) -> bool {
    card_face_gaps(face)
        .iter()
        .any(|gap| gap == UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP)
}

/// Rewrite every graveyard-to-exile mass move on the face to a different target.
fn set_mass_exile_target(face: &mut CardFace, replacement: TargetFilter) {
    for ability in &mut face.abilities {
        set_mass_exile_target_in(ability, &replacement);
    }
    for execute in face
        .triggers
        .iter_mut()
        .filter_map(|trigger| trigger.execute.as_deref_mut())
    {
        set_mass_exile_target_in(execute, &replacement);
    }
}

fn set_mass_exile_target_in(def: &mut AbilityDefinition, replacement: &TargetFilter) {
    if let Effect::ChangeZoneAll {
        origin: Some(Zone::Graveyard),
        destination: Zone::Exile,
        target,
        ..
    } = &mut *def.effect
    {
        *target = replacement.clone();
    }
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        set_mass_exile_target_in(sub, replacement);
    }
}

#[test]
fn assimilation_assessor_is_reported_unsupported_by_the_typed_assimilate_gap() {
    let assessor = face("Assimilation Assessor");
    assert!(
        has_opponent_graveyard_mass_exile(&assessor),
        "reach: opponent graveyard mass exile"
    );
    assert!(
        has_assimilate_pile_read(&assessor),
        "reach: linked assimilate read"
    );
    assert!(carries_gap(&assessor));

    let coverage = card_face_coverage(&assessor);
    assert!(coverage
        .gap_handlers()
        .iter()
        .any(|gap| gap == UNTARGETED_OPPONENT_PILE_ASSIMILATE_GAP));
    assert!(!coverage.is_supported());
}

#[test]
fn each_half_of_the_pair_is_load_bearing() {
    let printed = face("Assimilation Assessor");

    // (a) Without the assimilate reader trigger the label disappears.
    let mut without_reader = printed.clone();
    without_reader.triggers.retain(|trigger| {
        !trigger
            .execute
            .as_deref()
            .is_some_and(definition_has_assimilate_read)
    });
    assert!(
        has_opponent_graveyard_mass_exile(&without_reader),
        "reach retained: mass exile"
    );
    assert!(!has_assimilate_pile_read(&without_reader), "reader removed");
    assert!(!carries_gap(&without_reader), "without the reader");

    // (b) Without the opponent-scoped mass move the label disappears.
    let mut retargeted = printed.clone();
    set_mass_exile_target(&mut retargeted, TargetFilter::Player);
    assert!(
        has_assimilate_pile_read(&retargeted),
        "reach retained: reader"
    );
    assert!(
        !has_opponent_graveyard_mass_exile(&retargeted),
        "opponent scope replaced"
    );
    assert!(
        !carries_gap(&retargeted),
        "without the opponent-scoped mass move"
    );
}

#[test]
fn the_opponent_graveyard_mass_move_alone_does_not_carry_the_label() {
    let release = face("Release to Memory");
    assert!(
        has_opponent_graveyard_mass_exile(&release),
        "reach: opponent graveyard mass exile"
    );
    assert!(!has_assimilate_pile_read(&release), "no assimilate reader");
    assert!(!carries_gap(&release));
}

#[test]
fn faces_without_the_opponent_graveyard_move_do_not_carry_the_label() {
    for name in [
        "Borg Cube",
        "Mimic Vat",
        "Mysterious Limousine",
        "Duplicity",
        "Moonring Mirror",
        "Mirror of Life Trapping",
    ] {
        let printed = face(name);
        assert!(!carries_gap(&printed), "{name}");
    }
}
