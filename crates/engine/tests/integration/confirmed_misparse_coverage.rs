//! Coverage metadata changes support classification, never parsed rules.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, card_face_coverage_with_subtypes, card_face_gaps, CardCoverageResult,
    GapDetail, GapDiagnosis, ENTRY_CONSUMER_GAP, ENTRY_PROJECTION_GAP,
};
use engine::game::deck_loading::default_scheme_deck_entries;
use engine::game::deck_validation::{
    evaluate_deck_compatibility, DeckCompatibilityRequest, DeckCoverage, UnsupportedCard,
};
use engine::game::gap_analysis::{analyze_gaps, GapClass};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::confirmed_misparse::{ConfirmedMisparses, MisparseClass};
use engine::types::format::{GameFormat, SelectedFormat};
use serde_json::{Map, Value};
use strum::IntoEnumIterator;

fn fixture() -> &'static Map<String, Value> {
    static RAW: LazyLock<Map<String, Value>> = LazyLock::new(|| {
        // Regeneration requires scripts/gen-test-fixture.py and gzip -9 -n.
        let json = engine::database::integration_fixture::json().unwrap();
        serde_json::from_str(&json).unwrap()
    });
    &RAW
}

fn one_registry(rows: &[(&str, MisparseClass)]) -> ConfirmedMisparses {
    let entries: Vec<String> = rows
        .iter()
        .map(|(key, class)| {
            format!(
                "{{ key = {}, class = {}, clauses = [\"census-cited clause\"] }}",
                serde_json::to_string(key).unwrap(),
                serde_json::to_string(class).unwrap(),
            )
        })
        .collect();
    ConfirmedMisparses::parse(&format!("face = [{}]", entries.join(", "))).unwrap()
}

fn one_row(key: &str, class: MisparseClass) -> ConfirmedMisparses {
    one_registry(&[(key, class)])
}

/// Preserve the original export envelope and every unmodified value. Only the
/// marker produced by the production stamper is replaced, including removal.
fn stamped_export(raw: &Map<String, Value>, list: &ConfirmedMisparses) -> Map<String, Value> {
    let mut faces: BTreeMap<&str, CardFace> = raw
        .iter()
        .map(|(key, value)| (key.as_str(), serde_json::from_value(value.clone()).unwrap()))
        .collect();
    list.stamp_all(faces.iter_mut().map(|(key, face)| (*key, face)));
    let mut result = raw.clone();
    for (key, face) in faces {
        let entry = result.get_mut(key).unwrap().as_object_mut().unwrap();
        match face.metadata.confirmed_misparse {
            Some(class) => {
                let metadata = entry
                    .entry("metadata")
                    .or_insert_with(|| Value::Object(Map::new()));
                metadata.as_object_mut().unwrap().insert(
                    "confirmed_misparse".to_string(),
                    serde_json::to_value(class).unwrap(),
                );
            }
            None => {
                if let Some(metadata) = entry.get_mut("metadata") {
                    let metadata = metadata.as_object_mut().unwrap();
                    metadata.remove("confirmed_misparse");
                    if metadata.is_empty() {
                        entry.remove("metadata");
                    }
                }
            }
        }
    }
    result
}

fn database(raw: &Map<String, Value>) -> CardDatabase {
    CardDatabase::from_json_str(&serde_json::to_string(raw).unwrap()).unwrap()
}

fn report(db: &CardDatabase) -> BTreeMap<String, CardCoverageResult> {
    analyze_coverage(db)
        .cards
        .into_iter()
        .map(|card| (card.card_face_key.clone().expect("exact export key"), card))
        .collect()
}

fn handlers(card: &CardCoverageResult) -> Vec<&str> {
    card.gap_details
        .iter()
        .map(|gap| gap.handler.as_str())
        .collect()
}

#[test]
fn confirmed_misparse_parser_validates_exact_identity_and_schema() {
    let rows = r#"face = [
            { key = "same", class = "wrong_count", clauses = ["census-cited clause"] },
            { key = "same [id]", class = "missing_effect", clauses = ["other clause"] },
        ]"#;
    let list = ConfirmedMisparses::parse(rows).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.class_of("same"), Some(MisparseClass::WrongCount));
    assert_eq!(
        list.class_of("same [id]"),
        Some(MisparseClass::MissingEffect)
    );
    assert_eq!(list.class_of("Same"), None);
    assert_eq!(list.iter().next().unwrap().clauses, ["census-cited clause"]);
    assert!(ConfirmedMisparses::parse("face = []").unwrap().is_empty());
    for invalid in [
        r#"face = [{ key = "same", class = "other", clauses = [] }, { key = "same", class = "other", clauses = [] }]"#,
        r#"face = [{ key = "same", class = "unknown", clauses = [] }]"#,
        r#"face = [{ key = "same", class = "other", clauses = [], why = "not allowed" }]"#,
        "face = []\nunknown = true",
    ] {
        assert!(ConfirmedMisparses::parse(invalid).is_err(), "{invalid}");
    }
}
#[test]
fn confirmed_misparse_round_trip_and_clear_preserve_unlisted_export() {
    let raw: Map<String, Value> = ["lightning bolt", "plains"]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let old = database(&raw);
    assert_eq!(
        old.get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        None
    );
    let list = one_row("lightning bolt", MisparseClass::WrongCount);
    let marked_raw = stamped_export(&raw, &list);
    assert_eq!(marked_raw["plains"], raw["plains"]);
    let marked = database(&marked_raw);
    assert_eq!(
        marked
            .get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        Some(MisparseClass::WrongCount)
    );
    let cleared = stamped_export(&marked_raw, &ConfirmedMisparses::empty());
    assert_eq!(cleared, raw);
    assert_eq!(
        database(&cleared)
            .get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        None
    );

    let absent = one_row("absent exact key", MisparseClass::Other);
    let mut face: CardFace = serde_json::from_value(raw["lightning bolt"].clone()).unwrap();
    let outcome = absent.stamp_all([("lightning bolt", &mut face)]);
    assert_eq!(outcome.marked, 0);
    assert_eq!(outcome.unmatched_keys, ["absent exact key"]);
}

#[test]
fn confirmed_misparse_each_class_changes_both_coverage_authorities() {
    // All classes share one database: `analyze_coverage` has a large fixed cost per call.
    const POOL: usize = 60;
    let mut candidates: Vec<&String> = fixture().keys().collect();
    candidates.sort();
    candidates.truncate(POOL);
    let pool: Map<String, Value> = candidates
        .iter()
        .map(|key| (key.to_string(), fixture()[key.as_str()].clone()))
        .collect();
    let baseline = database(&stamped_export(&pool, &ConfirmedMisparses::empty()));
    let before = report(&baseline);
    let classes: Vec<MisparseClass> = MisparseClass::iter().collect();
    // Reach guards folded into selection: no hidden-key faces, and every chosen
    // card is baseline-supported, gap-free, and has abilities to parse.
    let chosen: Vec<&String> = candidates
        .iter()
        .copied()
        .filter(|key| !key.contains('['))
        .filter(|key| {
            let row = &before[key.as_str()];
            row.supported
                && row.gap_details.is_empty()
                && !baseline
                    .get_face_by_name(key.as_str())
                    .expect("pool face")
                    .abilities
                    .is_empty()
        })
        .take(classes.len())
        .collect();
    assert_eq!(
        chosen.len(),
        classes.len(),
        "first {POOL} fixture keys must supply one distinct gap-free card per MisparseClass"
    );
    let raw: Map<String, Value> = chosen
        .iter()
        .map(|key| (key.to_string(), fixture()[key.as_str()].clone()))
        .collect();
    let rows: Vec<(&str, MisparseClass)> = chosen
        .iter()
        .zip(&classes)
        .map(|(key, class)| (key.as_str(), *class))
        .collect();
    let marked = database(&stamped_export(&raw, &one_registry(&rows)));
    let after = report(&marked);
    for (key, class) in rows {
        let marked_row = &after[key];
        assert_eq!(
            class.handler(),
            format!(
                "Misparse:{}",
                serde_json::to_value(class).unwrap().as_str().unwrap()
            )
        );
        assert!(!marked_row.supported, "{key}: {class:?}");
        assert_eq!(handlers(marked_row), [class.handler()], "{key}: {class:?}");
        assert_eq!(
            card_face_gaps(marked.get_face_by_name(key).unwrap()),
            [class.handler()],
            "{key}: {class:?}"
        );
        assert_eq!(
            serde_json::to_value(&marked_row.parse_details).unwrap(),
            serde_json::to_value(&before[key].parse_details).unwrap(),
            "{key}: {class:?}"
        );
    }
}

/// Every face is compared; printed-name collisions cannot collapse rows.
fn compare_population(raw: &Map<String, Value>, populations: &[(&ConfirmedMisparses, &str)]) {
    let baseline = database(&stamped_export(raw, &ConfirmedMisparses::empty()));
    let before = report(&baseline);
    drop(baseline);
    assert_eq!(before.len(), raw.len());
    for &(list, label) in populations {
        let marked = database(&stamped_export(raw, list));
        let after = report(&marked);
        assert_eq!(after.len(), before.len());
        let mut previously_supported = 0;
        let mut already_unsupported = 0;
        let mut classes = BTreeMap::new();
        for (key, original) in &before {
            let changed = &after[key];
            assert_eq!(
                serde_json::to_value(&original.parse_details).unwrap(),
                serde_json::to_value(&changed.parse_details).unwrap(),
                "parse details: {key}"
            );
            if let Some(class) = list.class_of(key) {
                *classes.entry(class).or_insert(0usize) += 1;
                assert!(!changed.supported, "listed {key}");
                let mut expected = handlers(original);
                expected.push(class.handler());
                expected.sort_unstable();
                expected.dedup();
                assert_eq!(handlers(changed), expected, "listed gaps: {key}");
                assert!(
                    card_face_gaps(marked.get_face_by_name(key).unwrap())
                        .iter()
                        .any(|gap| gap == class.handler()),
                    "face gaps: {key}"
                );
                if original.supported {
                    previously_supported += 1;
                } else {
                    already_unsupported += 1;
                }
            } else {
                assert_eq!(
                    serde_json::to_value(original).unwrap(),
                    serde_json::to_value(changed).unwrap(),
                    "unlisted {key}"
                );
            }
        }
        assert!(!list.is_empty(), "{label}: population must be nonempty");
        assert!(
            previously_supported + already_unsupported > 0,
            "{label}: listed-present witness"
        );
        assert!(
            previously_supported > 0,
            "{label}: real false-green witness"
        );
        eprintln!("{label}: F={previously_supported} U={already_unsupported} per_class={classes:?} compared={}", before.len());
    }
}

#[test]
fn confirmed_misparse_fixture_population_and_unlisted_invariance() {
    compare_population(
        fixture(),
        &[
            (
                &one_row("lightning bolt", MisparseClass::WrongCount),
                "fixture constructed population",
            ),
            (
                ConfirmedMisparses::committed(),
                "fixture committed population",
            ),
        ],
    );
}

#[test]
fn confirmed_misparse_full_export_population_and_unlisted_invariance() {
    let Some(raw) = super::support::shared_card_export_json() else {
        return;
    };
    compare_population(
        raw,
        &[
            (
                &one_row("lightning bolt", MisparseClass::WrongCount),
                "full export constructed population",
            ),
            (
                ConfirmedMisparses::committed(),
                "full export committed population",
            ),
        ],
    );
}

#[test]
fn confirmed_misparse_full_compatibility_counts_unique_cards_without_changing_legality() {
    let raw: Map<String, Value> = ["lightning bolt", "plains", "absolving lammasu"]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let baseline = database(&stamped_export(&raw, &ConfirmedMisparses::empty()));
    assert!(card_face_gaps(baseline.get_face_by_name("Lightning Bolt").unwrap()).is_empty());
    assert!(card_face_gaps(baseline.get_face_by_name("Plains").unwrap()).is_empty());
    assert!(!card_face_gaps(baseline.get_face_by_name("Absolving Lammasu").unwrap()).is_empty());
    let mut main_deck = vec![
        "Lightning Bolt".to_string(),
        "Lightning Bolt".to_string(),
        "Absolving Lammasu".to_string(),
    ];
    main_deck.extend(std::iter::repeat_n("Plains".to_string(), 57));
    let request = DeckCompatibilityRequest {
        main_deck,
        selected_format: Some(SelectedFormat::Tag(GameFormat::Vintage)),
        ..Default::default()
    };
    let before = evaluate_deck_compatibility(&baseline, &request);
    assert_eq!(before.selected_format_compatible, Some(true));
    assert_eq!(before.format_legality["vintage"], "legal");
    let marked = database(&stamped_export(
        &raw,
        &one_row("lightning bolt", MisparseClass::WrongCount),
    ));
    let after = evaluate_deck_compatibility(&marked, &request);
    assert_eq!(before.format_legality, after.format_legality);
    assert_eq!(
        before.selected_format_compatible,
        after.selected_format_compatible
    );
    assert_eq!(
        before.selected_format_reasons,
        after.selected_format_reasons
    );
    assert_eq!(before.unknown_cards, after.unknown_cards);
    let before = before.coverage.unwrap();
    let after = after.coverage.unwrap();
    assert_eq!((before.supported_unique, before.total_unique), (2, 3));
    assert_eq!((after.supported_unique, after.total_unique), (1, 3));
    assert_eq!(
        after
            .unsupported_cards
            .iter()
            .map(|card| card.name.as_str())
            .collect::<Vec<_>>(),
        ["Absolving Lammasu", "Lightning Bolt"]
    );
    assert_eq!(
        serde_json::to_value(&before.unsupported_cards[0]).unwrap(),
        serde_json::to_value(&after.unsupported_cards[0]).unwrap()
    );
    assert_eq!(after.unsupported_cards[1].copies, 2);
    assert_eq!(after.unsupported_cards[1].gaps, ["Misparse:wrong_count"]);
}

/// CR 904.3: A scheme deck contains at least 20 Scheme cards. A scheme the
/// coverage authority holds unsupported is rejected by the same rule that
/// rejects any other unsupported scheme, so a marker must reach the deck gate.
#[test]
fn confirmed_misparse_scheme_rejects_marked_real_scheme() {
    const WITNESS: &str = "i call for slaughter";
    let witness = fixture()[WITNESS].clone();
    let mut raw: Map<String, Value> = Map::new();
    raw.insert("plains".to_string(), fixture()["plains"].clone());
    raw.insert(WITNESS.to_string(), witness.clone());
    // Control schemes share the witness's real face data under distinct names.
    for index in 1..=19 {
        let name = format!("Scheme {index}");
        let mut control = witness.clone();
        control["name"] = Value::String(name.clone());
        raw.insert(name.to_lowercase(), control);
    }
    let baseline = database(&stamped_export(&raw, &ConfirmedMisparses::empty()));
    let mut scheme_deck: Vec<String> = (1..=19).map(|index| format!("Scheme {index}")).collect();
    scheme_deck.push("I Call for Slaughter".to_string());
    for name in &scheme_deck {
        assert!(
            card_face_gaps(baseline.get_face_by_name(name).unwrap()).is_empty(),
            "{name} must be a gap-free control"
        );
    }
    let request = DeckCompatibilityRequest {
        main_deck: vec!["Plains".to_string(); 60],
        scheme_deck,
        selected_format: Some(SelectedFormat::Tag(GameFormat::Archenemy)),
        player_count: 4,
        ..Default::default()
    };
    let before = evaluate_deck_compatibility(&baseline, &request);
    assert_eq!(
        before.selected_format_compatible,
        Some(true),
        "{:?}",
        before.selected_format_reasons
    );

    let marked = database(&stamped_export(
        &raw,
        &one_row(WITNESS, MisparseClass::MissingEffect),
    ));
    let after = evaluate_deck_compatibility(&marked, &request);
    assert_eq!(after.selected_format_compatible, Some(false));
    assert_eq!(
        after.selected_format_reasons,
        ["Unsupported scheme cards: I Call for Slaughter"]
    );
}

#[test]
fn confirmed_misparse_hidden_key_is_not_a_printed_name_lookup() {
    let face = fixture()["lightning bolt"].clone();
    let raw: Map<String, Value> = [
        ("lightning bolt".to_string(), face.clone()),
        ("lightning bolt [hidden-id]".to_string(), face),
    ]
    .into_iter()
    .collect();
    let list = one_row("lightning bolt [hidden-id]", MisparseClass::WrongObject);
    let stamped = stamped_export(&raw, &list);
    assert_eq!(stamped["lightning bolt"], raw["lightning bolt"]);
    let db = database(&stamped);
    let results = report(&db);
    assert_eq!(results.len(), 2);
    assert!(results["lightning bolt"].supported);
    assert!(!results["lightning bolt [hidden-id]"].supported);
    assert_eq!(
        handlers(&results["lightning bolt [hidden-id]"]),
        ["Misparse:wrong_object"]
    );
    assert_eq!(
        db.face_iter()
            .find(|(key, _)| *key == "lightning bolt [hidden-id]")
            .unwrap()
            .1
            .name,
        "Lightning Bolt"
    );
}

#[test]
fn confirmed_misparse_removal_restores_only_marker_induced_support() {
    for key in ["lightning bolt", "a premonition of your demise"] {
        let raw: Map<String, Value> = [(key.to_string(), fixture()[key].clone())]
            .into_iter()
            .collect();
        let baseline = report(&database(&stamped_export(
            &raw,
            &ConfirmedMisparses::empty(),
        )));
        let marked_raw = stamped_export(&raw, &one_row(key, MisparseClass::MissingEffect));
        let marked = report(&database(&marked_raw));
        assert!(!marked[key].supported);
        assert!(handlers(&marked[key]).contains(&"Misparse:missing_effect"));
        let restored = report(&database(&stamped_export(
            &marked_raw,
            &ConfirmedMisparses::empty(),
        )));
        assert_eq!(
            serde_json::to_value(&restored[key]).unwrap(),
            serde_json::to_value(&baseline[key]).unwrap()
        );
        assert_eq!(restored[key].supported, key == "lightning bolt");
        if key != "lightning bolt" {
            assert!(!baseline[key].gap_details.is_empty());
            assert!(handlers(&baseline[key])
                .iter()
                .all(|gap| handlers(&marked[key]).contains(gap)));
        }
    }
}

fn eligible_schemes(db: &CardDatabase) -> usize {
    db.face_iter()
        .filter(|(_, face)| {
            face.card_type.core_types.contains(&CoreType::Scheme) && card_face_gaps(face).is_empty()
        })
        .count()
}

fn assert_default_schemes(db: &CardDatabase) {
    let selected = default_scheme_deck_entries(db);
    assert_eq!(selected.len(), 20);
    for entry in selected {
        assert_eq!(entry.count, 1);
        assert!(entry.card.card_type.core_types.contains(&CoreType::Scheme));
        assert!(
            card_face_gaps(&entry.card).is_empty(),
            "{}",
            entry.card.name
        );
    }
}

#[test]
fn confirmed_misparse_default_schemes_full_export() {
    let Some(raw) = super::support::shared_card_export_json() else {
        return;
    };
    let committed = ConfirmedMisparses::committed();
    assert!(!committed.is_empty(), "committed list must have rows");
    let mut present = 0;
    let mut listed_schemes = 0;
    for row in committed.iter() {
        if let Some(value) = raw.get(&row.key) {
            let face: CardFace = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                face.metadata.confirmed_misparse,
                Some(row.class),
                "export must already carry marker: {}",
                row.key
            );
            present += 1;
            if face.card_type.core_types.contains(&CoreType::Scheme) {
                assert!(card_face_gaps(&face)
                    .iter()
                    .any(|gap| gap == row.class.handler()));
                listed_schemes += 1;
            }
        }
    }
    assert!(present > 0, "full export must contain committed markers");
    let baseline = database(&stamped_export(raw, &ConfirmedMisparses::empty()));
    let supported_before = eligible_schemes(&baseline);
    let excluded_schemes: Vec<_> = baseline
        .face_iter()
        .filter(|(key, face)| {
            committed.class_of(key).is_some()
                && face.card_type.core_types.contains(&CoreType::Scheme)
                && card_face_gaps(face).is_empty()
        })
        .map(|(key, _)| key.to_string())
        .collect();
    // Deleting the last listed Scheme row must not fail this test; the constructed row below
    // proves the marker reaches the default scheme deck whatever the census list still holds.
    let witness = baseline
        .get_face_by_name("I Call for Slaughter")
        .expect("real full-export Scheme witness");
    assert!(witness.card_type.core_types.contains(&CoreType::Scheme));
    assert!(
        card_face_gaps(witness).is_empty(),
        "real Scheme must start eligible"
    );
    let constructed = database(&stamped_export(
        raw,
        &one_row("i call for slaughter", MisparseClass::MissingEffect),
    ));
    assert_eq!(eligible_schemes(&constructed), supported_before - 1);
    assert_eq!(
        card_face_gaps(
            constructed
                .get_face_by_name("I Call for Slaughter")
                .unwrap()
        ),
        ["Misparse:missing_effect"]
    );
    assert_default_schemes(&constructed);
    drop(constructed);
    drop(baseline);
    let marked = database(&stamped_export(raw, committed));
    let supported_after = eligible_schemes(&marked);
    assert_eq!(supported_after, supported_before - excluded_schemes.len());
    for key in &excluded_schemes {
        let face = marked.get_face_by_name(key).unwrap();
        let class = committed.class_of(key).unwrap();
        assert_eq!(face.metadata.confirmed_misparse, Some(class));
        assert_eq!(card_face_gaps(face), [class.handler()]);
    }
    assert_default_schemes(&marked);
    eprintln!("confirmed_misparse_default_schemes_full_export executed full export: committed_rows={} present_markers={present} listed_real_schemes={listed_schemes} excluded_eligible_schemes={} supported_before={supported_before} supported_after={supported_after} selected=20", committed.len(), excluded_schemes.len());
}

/// Production hookup: `analyze_coverage` itself attaches the census clauses and one
/// typed `Misparse` diagnosis per clause to a confirmed-misparse face, and the gap
/// analysis groups that row under the `misparse:<class>` category. Removing the
/// `explain_blank_gaps` call from `analyze_coverage` fails this test.
#[test]
fn analyze_coverage_names_the_census_clauses_and_gap_analysis_groups_them() {
    let key = "i call for slaughter";
    let entry = ConfirmedMisparses::committed()
        .get(key)
        .expect("the committed list carries this face");
    assert_eq!(entry.class, MisparseClass::MissingEffect);
    assert_eq!(entry.clauses, ["They gain haste until end of turn."]);
    let raw: Map<String, Value> = [key]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let db = database(&stamped_export(&raw, ConfirmedMisparses::committed()));
    let cards = analyze_coverage(&db).cards;
    let card = cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some(key))
        .expect("exact export key");
    let gap = card
        .gap_details
        .iter()
        .find(|gap| gap.handler == entry.class.handler())
        .expect("the marked face reports its misparse gap");
    assert_eq!(
        gap.source_text.as_deref(),
        Some(entry.clauses.join(" / ").as_str())
    );
    let expected: Vec<GapDiagnosis> = entry
        .clauses
        .iter()
        .map(|clause| GapDiagnosis::Misparse {
            class: entry.class,
            clause: clause.clone(),
        })
        .collect();
    assert_eq!(gap.diagnoses, expected);

    let analysis = analyze_gaps(&cards);
    let category = analysis
        .categories
        .get(&GapClass::Misparse(entry.class).label())
        .expect("the misparse category is reported");
    assert_eq!(category.tally.cards_affected, 1);
    assert!(!category.families.is_empty());
    assert!(category
        .families
        .iter()
        .all(|family| family.tally.cards_affected == 1));
    assert!(
        !analysis.categories.contains_key("undiagnosed"),
        "the misparse row is diagnosed, not undiagnosed"
    );
}

/// Production hookup for swallow text: Isildur's Fateful Strike opens with a
/// reminder line (line 0) that the audit unit absorbs; its swallow warnings flag
/// the printed clause on line 1. The exported text is that clause, never the
/// reminder.
#[test]
fn analyze_coverage_swallow_text_is_the_flagged_clause_not_the_absorbed_reminder() {
    let key = "isildur's fateful strike";
    let oracle = fixture()[key]["oracle_text"].as_str().unwrap();
    let clause = oracle.lines().nth(1).unwrap().trim();
    assert!(oracle.lines().next().unwrap().starts_with('('));
    let raw: Map<String, Value> = [key]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let cards = analyze_coverage(&database(&raw)).cards;
    let card = cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some(key))
        .expect("exact export key");
    let swallows: Vec<_> = card
        .gap_details
        .iter()
        .filter(|gap| gap.handler.starts_with("Swallow:"))
        .collect();
    assert!(!swallows.is_empty(), "the card carries swallow gaps");
    for gap in swallows {
        assert_eq!(gap.source_text.as_deref(), Some(clause), "{}", gap.handler);
    }
}

/// Cut Short's real export shape, through `analyze_coverage`: its `Duration_ThisTurn`
/// warning flags the destroy clause on line 1, but the warning's display text holds
/// only line 0 (the Convoke reminder), cut at 140 bytes. Rebuilding the clause from
/// that text names the supported Convoke keyword; the exported text must be the
/// destroy clause. The face is a SYNTHETIC carrier: the Lightning Bolt export entry
/// with Cut Short's name, its Oracle text and its warning, all verbatim from the card
/// database export (Cut Short is not in the committed fixture).
#[test]
fn analyze_coverage_swallow_text_for_cut_short_is_the_destroy_clause_not_convoke() {
    const ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)
Destroy target planeswalker that was activated this turn or tapped creature.";
    let mut face = fixture()["lightning bolt"].clone();
    let entry = face.as_object_mut().unwrap();
    entry.insert("name".into(), Value::String("Cut Short".into()));
    entry.insert("oracle_text".into(), Value::String(ORACLE.into()));
    entry.insert(
        "parse_warnings".into(),
        serde_json::json!([{
            "type": "SwallowedClause",
            "detector": "Duration_ThisTurn",
            "description": "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's",
            "line_index": 1,
            "unit_span": {
                "first_line": 0, "last_line": 1, "start_byte": 0, "end_byte": 225,
                "precision": "Exact", "ordinal_within_span": 0
            },
            "items": [0]
        }]),
    );
    let raw: Map<String, Value> = [("cut short".to_string(), face)].into_iter().collect();
    let cards = analyze_coverage(&database(&raw)).cards;
    let card = cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some("cut short"))
        .expect("exact export key");
    let gap = card
        .gap_details
        .iter()
        .find(|gap| gap.handler == "Swallow:Duration_ThisTurn")
        .expect("the warning is reported as a swallow gap");
    assert_eq!(
        gap.source_text.as_deref(),
        Some("Destroy target planeswalker that was activated this turn or tapped creature.")
    );
}

// ---------------------------------------------------------------------------
// Deck compatibility gap details
//
// `evaluate_deck_compatibility` feeds the deck editor's unsupported-card panel.
// Its `gaps` stay the bare handler labels, while `gap_details` is the same
// published-report shape the coverage dashboard reads: handler plus the printed
// clause (or unhandled feature) that makes the card unsupported.
// ---------------------------------------------------------------------------

const ZUR_KEY: &str = "zur the enchanter";
/// Verbatim Dominaria Remastered Oracle text (Scryfall, oracle id
/// `d7950018-d744-48a8-81aa-0d8384703f48`).
const ZUR_ORACLE: &str = "Flying\nWhenever Zur attacks, you may search your library for an enchantment card with mana value 3 or less, put it onto the battlefield, then shuffle.";
/// The clause the census cites for Zur the Enchanter, verbatim from
/// `crates/phase-ast/data/confirmed-misparses.toml`. It is also the trigger's
/// printed Oracle line, because the card names itself by a shortened form.
const ZUR_CENSUS_CLAUSE: &str = "Whenever Zur attacks, you may search your library for an enchantment card with mana value 3 or less, put it onto the battlefield, then shuffle.";

/// Zur the Enchanter is not in the committed fixture, so build its face through
/// the production MTGJSON -> Oracle path. Every printed field is verbatim from
/// the Dominaria Remastered printing: {1}{W}{U}{B}, 1/4, Legendary Creature —
/// Human Wizard, Flying. Parsing through `build_oracle_face` keeps the trigger
/// shaped exactly as card data ships it, so the coverage verdict is the shipping
/// one.
fn zur_the_enchanter_face() -> CardFace {
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers, SetRelatedCards};
    let card = AtomicCard {
        name: "Zur the Enchanter".to_string(),
        mana_cost: Some("{1}{W}{U}{B}".to_string()),
        colors: vec!["B".to_string(), "U".to_string(), "W".to_string()],
        color_identity: vec!["B".to_string(), "U".to_string(), "W".to_string()],
        power: Some("1".to_string()),
        toughness: Some("4".to_string()),
        loyalty: None,
        defense: None,
        text: Some(ZUR_ORACLE.to_string()),
        layout: "normal".to_string(),
        type_line: Some("Legendary Creature — Human Wizard".to_string()),
        types: vec!["Creature".to_string()],
        subtypes: vec!["Human".to_string(), "Wizard".to_string()],
        supertypes: vec!["Legendary".to_string()],
        keywords: Some(vec!["Flying".to_string()]),
        side: None,
        face_name: None,
        mana_value: 4.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some("d7950018-d744-48a8-81aa-0d8384703f48".to_string()),
            scryfall_id: Some("561cfc04-65ea-49a4-8638-b4631a7cf828".to_string()),
        },
        foreign_data: Vec::new(),
        related_cards: SetRelatedCards::default(),
    };
    engine::database::synthesis::build_oracle_face(&card, None)
}

/// Zur's export face stamped by the committed census list, keyed by the exact
/// export key the census lookup uses. The `marked` count is the reach guard: a
/// census key/spelling drift would stamp nothing, and this test would otherwise
/// degrade into "the unmarked card is supported".
fn stamped_zur_export() -> Value {
    let mut face = zur_the_enchanter_face();
    let stamp = ConfirmedMisparses::committed().stamp_all(std::iter::once((ZUR_KEY, &mut face)));
    assert_eq!(
        stamp.marked, 1,
        "the committed census must key Zur the Enchanter by the exact export key {ZUR_KEY}"
    );
    serde_json::to_value(&face).expect("stamped Zur the Enchanter face serializes")
}

fn deck_database(entries: impl IntoIterator<Item = (String, Value)>) -> CardDatabase {
    let raw: Map<String, Value> = entries.into_iter().collect();
    database(&raw)
}

/// The deck path publishes WHAT the confirmed misparse got wrong, not just the
/// handler label: the census clauses joined with " / " and one typed diagnosis
/// per clause. Before `UnsupportedCard::gap_details`, the deck editor received
/// only `Misparse:wrong_object`.
#[test]
fn deck_compatibility_confirmed_misparse_publishes_census_clause() {
    let entry = ConfirmedMisparses::committed()
        .get(ZUR_KEY)
        .expect("the committed list carries Zur the Enchanter");
    assert_eq!(entry.class, MisparseClass::WrongObject);
    assert_eq!(
        entry.clauses,
        [ZUR_CENSUS_CLAUSE],
        "the pinned clause must match the committed census row"
    );

    let db = deck_database([(ZUR_KEY.to_string(), stamped_zur_export())]);
    let request = DeckCompatibilityRequest {
        main_deck: vec!["Zur the Enchanter".to_string()],
        ..Default::default()
    };
    let coverage = evaluate_deck_compatibility(&db, &request)
        .coverage
        .expect("deck compatibility reports coverage");
    assert_eq!((coverage.total_unique, coverage.supported_unique), (1, 0));
    assert_eq!(coverage.unsupported_cards.len(), 1);
    let unsupported = &coverage.unsupported_cards[0];
    assert_eq!(unsupported.name, "Zur the Enchanter");
    assert_eq!(unsupported.gaps, ["Misparse:wrong_object"]);
    assert_eq!(
        unsupported.gap_details.len(),
        1,
        "gaps and gap_details must describe the same set: {:?}",
        unsupported.gaps
    );
    let detail = &unsupported.gap_details[0];
    assert_eq!(detail.handler, "Misparse:wrong_object");
    let expected_text = entry.clauses.join(" / ");
    assert_eq!(detail.source_text.as_deref(), Some(expected_text.as_str()));
    assert_eq!(
        detail.diagnoses,
        [GapDiagnosis::Misparse {
            class: MisparseClass::WrongObject,
            clause: ZUR_CENSUS_CLAUSE.to_string(),
        }],
        "the published gap carries the typed census diagnosis"
    );
}

/// No published deck gap leaves `source_text` blank. The two witnesses are
/// printed fixture faces already pinned as unsupported by
/// `deck_validation`'s `deck_coverage_matches_report_for_printed_warning_and_silent_drop_witnesses`:
/// Hinder's swallow warning (whose runtime text names the detector only) and
/// Druid of the Emerald Grove's silent drop.
#[test]
fn deck_compatibility_gap_details_name_every_gap_source() {
    const WITNESSES: [(&str, &str); 2] = [
        ("Druid of the Emerald Grove", "SilentDrop:1_of_2"),
        ("Hinder", "Swallow:Replacement_Instead"),
    ];
    let raw: Map<String, Value> = WITNESSES
        .iter()
        .map(|(name, _)| {
            let key = name.to_lowercase();
            (key.clone(), fixture()[key.as_str()].clone())
        })
        .collect();
    let db = database(&raw);
    let request = DeckCompatibilityRequest {
        main_deck: WITNESSES.iter().map(|(name, _)| name.to_string()).collect(),
        ..Default::default()
    };
    let coverage = evaluate_deck_compatibility(&db, &request)
        .coverage
        .expect("deck compatibility reports coverage");
    assert_eq!((coverage.total_unique, coverage.supported_unique), (2, 0));
    assert_eq!(coverage.unsupported_cards.len(), 2);
    for (name, expected_gap) in WITNESSES {
        let card = coverage
            .unsupported_cards
            .iter()
            .find(|card| card.name == name)
            .unwrap_or_else(|| panic!("{name} must be reported unsupported"));
        assert_eq!(card.gaps, [expected_gap]);
        assert_eq!(
            card.gap_details
                .iter()
                .map(|gap| gap.handler.as_str())
                .collect::<Vec<_>>(),
            [expected_gap],
            "{name}: gap_details must mirror gaps in order"
        );
        for gap in &card.gap_details {
            let text = gap.source_text.as_deref().unwrap_or_else(|| {
                panic!("{name}: {} must publish the text it concerns", gap.handler)
            });
            assert!(
                !text.trim().is_empty(),
                "{name}: {} publishes blank source text",
                gap.handler
            );
        }
    }
    let hinder = coverage
        .unsupported_cards
        .iter()
        .find(|card| card.name == "Hinder")
        .expect("Hinder is reported unsupported");
    let swallow_text = hinder.gap_details[0]
        .source_text
        .as_deref()
        .expect("the swallow gap names its printed clause");
    assert!(
        !swallow_text.starts_with("[warning:")
            && swallow_text.contains("top or bottom of its owner's library"),
        "the swallow source text is the printed rider, not the detector display: {swallow_text}"
    );
}

/// The coverage report and the deck verdict share one explained-gap authority:
/// for each unsupported face, `explained_gap_details` names the same handlers in
/// the same order as `gap_handlers`, and the deck row's `gap_details` serialize
/// byte-for-byte like the published report row's. A drift in either path (the
/// clause lookup, the blank-text fallback, the key) fails here.
#[test]
fn deck_gap_details_match_report_and_face_handlers() {
    let db = deck_database([
        (
            "druid of the emerald grove".to_string(),
            fixture()["druid of the emerald grove"].clone(),
        ),
        ("hinder".to_string(), fixture()["hinder"].clone()),
        (ZUR_KEY.to_string(), stamped_zur_export()),
    ]);
    let request = DeckCompatibilityRequest {
        main_deck: vec![
            "Zur the Enchanter".to_string(),
            "Hinder".to_string(),
            "Druid of the Emerald Grove".to_string(),
        ],
        ..Default::default()
    };
    let coverage = evaluate_deck_compatibility(&db, &request)
        .coverage
        .expect("deck compatibility reports coverage");
    assert_eq!(coverage.unsupported_cards.len(), 3);
    let report = report(&db);
    for card in &coverage.unsupported_cards {
        let key = card.name.to_lowercase();
        let face = db
            .get_face_by_name(&card.name)
            .unwrap_or_else(|| panic!("{key} resolves in the deck database"));
        let face_coverage = card_face_coverage_with_subtypes(face, db.valid_subtypes());
        let explained = face_coverage.explained_gap_details(&key, face);
        assert_eq!(
            explained
                .iter()
                .map(|gap| gap.handler.clone())
                .collect::<Vec<_>>(),
            face_coverage.gap_handlers(),
            "{key}: explained gaps keep the gap order"
        );
        let row = report
            .get(&key)
            .unwrap_or_else(|| panic!("{key} is in the published report"));
        assert!(!row.supported, "{key}: the report agrees it is unsupported");
        assert_eq!(
            serde_json::to_value(&row.gap_details).unwrap(),
            serde_json::to_value(&card.gap_details).unwrap(),
            "{key}: report and deck paths must publish one gap shape"
        );
    }
}

/// Old deck-compatibility payloads (no `gap_details`) still deserialize, and an
/// empty list stays off the wire, so the field is additive for every consumer.
#[test]
fn unsupported_card_gap_details_are_additive_on_the_wire() {
    let legacy = serde_json::json!({
        "name": "Hinder",
        "gaps": ["Swallow:Replacement_Instead"],
    });
    let card: UnsupportedCard =
        serde_json::from_value(legacy).expect("the pre-gap_details shape still deserializes");
    assert!(card.gap_details.is_empty());
    assert_eq!(card.gaps, ["Swallow:Replacement_Instead"]);
    assert_eq!(card.copies, 1, "copies keeps its serde default");
    let reserialized = serde_json::to_value(&card).expect("UnsupportedCard serializes");
    assert!(
        reserialized.get("gap_details").is_none(),
        "an empty gap_details list is omitted: {reserialized}"
    );
    assert_eq!(reserialized["name"], "Hinder");
}

// ---------------------------------------------------------------------------
// Deck compatibility gap details: printed-card witnesses
//
// Every card below is a printed face from the committed integration fixture,
// with its verbatim Oracle text. The census clauses are asserted against the
// committed `confirmed-misparses.toml` AND against the face's printed text, so a
// paraphrase or a drifted census row fails here instead of passing vacuously.
// ---------------------------------------------------------------------------

/// A database holding exactly the named printed fixture faces (fixture keys), so
/// one `analyze_coverage` call over it stays cheap.
fn printed_pool(keys: &[&str]) -> CardDatabase {
    deck_database(keys.iter().map(|key| {
        let face = fixture()
            .get(*key)
            .unwrap_or_else(|| panic!("the integration fixture carries {key}"));
        (key.to_string(), face.clone())
    }))
}

/// The verdict's coverage for a deck of the named cards, through the production
/// entry point.
fn verdict_coverage(db: &CardDatabase, deck: &[&str]) -> DeckCoverage {
    let request = DeckCompatibilityRequest {
        main_deck: deck.iter().map(|name| name.to_string()).collect(),
        ..Default::default()
    };
    evaluate_deck_compatibility(db, &request)
        .coverage
        .expect("deck compatibility reports coverage")
}

fn unsupported_named<'a>(coverage: &'a DeckCoverage, name: &str) -> &'a UnsupportedCard {
    coverage
        .unsupported_cards
        .iter()
        .find(|card| card.name == name)
        .unwrap_or_else(|| {
            panic!(
                "{name} must be reported unsupported, got {:?}",
                coverage
                    .unsupported_cards
                    .iter()
                    .map(|card| card.name.as_str())
                    .collect::<Vec<_>>()
            )
        })
}

fn printed_oracle(db: &CardDatabase, name: &str) -> String {
    db.get_face_by_name(name)
        .and_then(|face| face.oracle_text.clone())
        .unwrap_or_else(|| panic!("{name} is a printed face with Oracle text"))
}

/// The card's single census gap: expected values come from the printed clauses
/// (pinned against both the committed census row and the face's Oracle text), not
/// from the coverage report.
fn assert_census_gap(
    db: &CardDatabase,
    card: &UnsupportedCard,
    key: &str,
    class: MisparseClass,
    clauses: &[&str],
) {
    let entry = ConfirmedMisparses::committed()
        .get(key)
        .unwrap_or_else(|| panic!("the committed census carries {key}"));
    assert_eq!(entry.class, class, "{key}: census class");
    assert_eq!(entry.clauses, clauses, "{key}: pinned census clauses");
    let oracle = printed_oracle(db, &card.name);
    for clause in clauses {
        assert!(
            oracle.contains(clause),
            "{key}: the census clause is printed Oracle text: {clause}"
        );
    }
    assert!(card.gaps.iter().any(|gap| gap == class.handler()), "{key}");
    let census: Vec<&GapDetail> = card
        .gap_details
        .iter()
        .filter(|gap| gap.handler == class.handler())
        .collect();
    assert_eq!(census.len(), 1, "{key}: one census gap in {:?}", card.gaps);
    assert_eq!(
        census[0].source_text.as_deref(),
        Some(clauses.join(" / ").as_str()),
        "{key}: the gap names the census clauses, joined in census order"
    );
    assert_eq!(
        census[0].diagnoses,
        clauses
            .iter()
            .map(|clause| GapDiagnosis::Misparse {
                class,
                clause: clause.to_string(),
            })
            .collect::<Vec<_>>(),
        "{key}: one typed diagnosis per census clause, in order"
    );
}

/// Multi-clause census gaps reach the deck verdict: the census clauses, joined
/// with " / ", and one typed diagnosis per clause. Zurgo and Ojutai and Abandon
/// Hope each cite two clauses, Aang's Journey one. Reach guards: Plains and
/// Lightning Bolt (supported) are counted but not listed, so the three
/// unsupported rows are not an upstream short circuit.
#[test]
fn deck_compatibility_publishes_multi_clause_census_gaps_for_printed_cards() {
    let db = printed_pool(&[
        "plains",
        "lightning bolt",
        "zurgo and ojutai",
        "abandon hope",
        "aang's journey",
    ]);
    let coverage = verdict_coverage(
        &db,
        &[
            "Plains",
            "Lightning Bolt",
            "Zurgo and Ojutai",
            "Abandon Hope",
            "Aang's Journey",
        ],
    );
    assert_eq!((coverage.total_unique, coverage.supported_unique), (5, 2));
    assert_eq!(coverage.unsupported_cards.len(), 3);
    for supported in ["Plains", "Lightning Bolt"] {
        assert!(
            !coverage
                .unsupported_cards
                .iter()
                .any(|card| card.name == supported),
            "{supported} is supported and carries no gap"
        );
    }
    assert_census_gap(
        &db,
        unsupported_named(&coverage, "Zurgo and Ojutai"),
        "zurgo and ojutai",
        MisparseClass::DroppedQualifier,
        &[
            "Put one of them into your hand and the rest on the bottom of your library in any order.",
            "You may return one of those Dragons to its owner's hand.",
        ],
    );
    assert_census_gap(
        &db,
        unsupported_named(&coverage, "Abandon Hope"),
        "abandon hope",
        MisparseClass::WrongCount,
        &[
            "Look at target opponent's hand and choose X cards from it.",
            "That player discards those cards.",
        ],
    );
    assert_census_gap(
        &db,
        unsupported_named(&coverage, "Aang's Journey"),
        "aang's journey",
        MisparseClass::ExtraEffect,
        &["Search your library for a basic land card. If this spell was kicked, instead search your library for a basic land card and a Shrine card."],
    );
    for card in &coverage.unsupported_cards {
        assert_eq!(
            card.gaps,
            card.gap_details
                .iter()
                .map(|gap| gap.handler.clone())
                .collect::<Vec<_>>(),
            "{}: gaps and gap_details list the same handlers in the same order",
            card.name
        );
    }
}

/// A deck that spells a card by an alias or a composite name resolves to the
/// same export key the report uses, so the census lookup still finds its
/// clauses. A key built from the raw spelling ("lim-dul's paladin") would
/// miss the census row and fall back to a generic handler line, failing the
/// clause assertions below.
#[test]
fn deck_compatibility_census_clauses_survive_alias_and_composite_spellings() {
    const PALADIN_CLAUSE: &str = "Whenever this creature attacks and isn't blocked, it assigns no combat damage this turn and defending player loses 4 life.";
    const ANNEX_CLAUSES: [&str; 2] = [
        "If you control a Demon, each opponent loses 2 life and you gain 2 life.",
        "Otherwise, you lose 2 life.",
    ];
    let db = printed_pool(&["plains", "lim-dûl's paladin", "unholy annex"]);

    // Exact spelling first: the reference for the alias and composite rows.
    let exact_paladin = verdict_coverage(&db, &["Lim-Dûl's Paladin"]);
    let exact_annex = verdict_coverage(&db, &["Unholy Annex"]);
    assert_census_gap(
        &db,
        unsupported_named(&exact_paladin, "Lim-Dûl's Paladin"),
        "lim-dûl's paladin",
        MisparseClass::MissingEffect,
        &[PALADIN_CLAUSE],
    );
    assert_census_gap(
        &db,
        unsupported_named(&exact_annex, "Unholy Annex"),
        "unholy annex",
        MisparseClass::WrongPlayer,
        &ANNEX_CLAUSES,
    );

    // Unaccented alias: `fold_card_name_key` resolves it to the accented key.
    let alias = verdict_coverage(&db, &["Lim-Dul's Paladin"]);
    assert_eq!(alias.unsupported_cards.len(), 1);
    assert_eq!(
        serde_json::to_value(&alias.unsupported_cards[0]).unwrap(),
        serde_json::to_value(&exact_paladin.unsupported_cards[0]).unwrap(),
        "an alias spelling publishes the same unsupported card"
    );
    // Composite spelling: the front face names the stored face.
    let composite = verdict_coverage(&db, &["Unholy Annex // Ritual Chamber"]);
    assert_eq!(composite.unsupported_cards.len(), 1);
    assert_eq!(
        serde_json::to_value(&composite.unsupported_cards[0]).unwrap(),
        serde_json::to_value(&exact_annex.unsupported_cards[0]).unwrap(),
        "a composite spelling publishes the same unsupported card"
    );

    // One card under two spellings is one entry, counted twice, with one list.
    let both = verdict_coverage(&db, &["Lim-Dul's Paladin", "Lim-Dûl's Paladin"]);
    assert_eq!(both.unsupported_cards.len(), 1);
    assert_eq!(both.unsupported_cards[0].copies, 2);
    assert_eq!(both.unsupported_cards[0].gap_details.len(), 1);

    // The alias row equals the report's row for the stored key.
    let report = report(&db);
    assert_eq!(
        serde_json::to_value(&report["lim-dûl's paladin"].gap_details).unwrap(),
        serde_json::to_value(&alias.unsupported_cards[0].gap_details).unwrap()
    );

    // Hostile: a name no step resolves yields no entry and no panic, beside a
    // resolvable unsupported card that does produce one.
    let unknown = verdict_coverage(&db, &["Lim-Dul's Paladin", "Totally Not A Real Card Xyzzy"]);
    assert_eq!(unknown.total_unique, 1);
    assert_eq!(unknown.unsupported_cards.len(), 1);
    assert_eq!(unknown.unsupported_cards[0].name, "Lim-Dûl's Paladin");
}

/// The one intended divergence from the report: entry-projection gaps (CR 614.12,
/// `EntryProjectionPolicy::Include`) are deck-only. Arcane Adaptation's printed
/// as-enters choice is unsupported in the verdict with a published entry gap, but
/// supported in the report; Metallic Mimic, the allowlisted same shape, is
/// supported in both.
#[test]
fn deck_compatibility_entry_projection_gap_is_deck_only_and_explained() {
    let db = printed_pool(&["plains", "arcane adaptation", "metallic mimic"]);
    assert!(
        printed_oracle(&db, "Arcane Adaptation")
            .contains("As this enchantment enters, choose a creature type."),
        "reach: the printed as-enters choice is the entry projection"
    );
    let coverage = verdict_coverage(&db, &["Plains", "Arcane Adaptation", "Metallic Mimic"]);
    assert_eq!((coverage.total_unique, coverage.supported_unique), (3, 2));
    assert_eq!(coverage.unsupported_cards.len(), 1);
    let arcane = unsupported_named(&coverage, "Arcane Adaptation");
    assert_eq!(arcane.gaps, [ENTRY_PROJECTION_GAP]);
    assert_eq!(arcane.gap_details.len(), 1);
    assert_eq!(arcane.gap_details[0].handler, ENTRY_PROJECTION_GAP);
    // The two deck-only entry labels have no printed clause to cite: the report
    // never emits them, so `blank_gap_text` explains the projection limit in
    // plain English instead of falling back to the handler label.
    assert_eq!(
        arcane.gap_details[0].source_text.as_deref(),
        Some(
            "As this permanent enters, the engine cannot yet apply the changes its entering \
             choice makes to other permanents (CR 614.12)"
        ),
        "the deck-only gap carries a specific explanation: {:?}",
        arcane.gap_details[0]
    );

    let report = report(&db);
    assert!(
        report["arcane adaptation"].supported && report["arcane adaptation"].gap_details.is_empty(),
        "the report excludes the entry-projection check by policy"
    );
    assert!(report["metallic mimic"].supported);
}

/// Byte-for-byte parity between the verdict and the report across a pool of
/// printed faces: the named witnesses of every gap family plus a bounded sorted
/// slice of other fixture faces. Removing exactly the two deck-only entry
/// labels from a verdict list leaves JSON equal to the report row, and the
/// census cards also match the clauses derived from the committed census (not
/// only the report).
#[test]
fn deck_gap_details_equal_report_for_every_unsupported_face_in_a_printed_pool() {
    const WITNESSES: [&str; 13] = [
        "plains",
        "lightning bolt",
        "metallic mimic",
        "zurgo and ojutai",
        "abandon hope",
        "aang's journey",
        "hinder",
        "druid of the emerald grove",
        "transcendent dragon",
        "absolving lammasu",
        "arcane adaptation",
        "gríma, saruman's footman",
        "unholy annex",
    ];
    let mut sorted: Vec<&String> = fixture().keys().collect();
    sorted.sort();
    let mut keys: BTreeSet<&str> = WITNESSES.into_iter().collect();
    // Selection guard: the key is the lowercased printed name, so a verdict row
    // (named by `face.name`) finds its report row by key.
    keys.extend(
        sorted
            .iter()
            .map(|key| key.as_str())
            .filter(|key| !key.contains('['))
            .filter(|key| {
                fixture()[*key]["name"]
                    .as_str()
                    .is_some_and(|name| name.to_lowercase() == *key)
            })
            .take(60),
    );
    let key_list: Vec<&str> = keys.iter().copied().collect();
    let db = printed_pool(&key_list);
    let deck: Vec<&str> = key_list
        .iter()
        .map(|key| fixture()[*key]["name"].as_str().expect("printed name"))
        .collect();
    let coverage = verdict_coverage(&db, &deck);
    assert_eq!(coverage.total_unique, key_list.len());
    let report = report(&db);

    let is_deck_only =
        |gap: &GapDetail| gap.handler == ENTRY_PROJECTION_GAP || gap.handler == ENTRY_CONSUMER_GAP;
    let mut families: BTreeSet<&str> = BTreeSet::new();
    let mut deck_only_cards: Vec<&str> = Vec::new();
    let mut census_cards = 0;
    for card in &coverage.unsupported_cards {
        let key = card.name.to_lowercase();
        let row = report
            .get(&key)
            .unwrap_or_else(|| panic!("{key} is in the published report"));
        assert!(!card.gaps.is_empty(), "{key}: an unsupported card has gaps");
        assert_eq!(
            card.gaps,
            card.gap_details
                .iter()
                .map(|gap| gap.handler.clone())
                .collect::<Vec<_>>(),
            "{key}: gaps and gap_details list the same handlers in the same order"
        );
        let shared: Vec<&GapDetail> = card
            .gap_details
            .iter()
            .filter(|gap| !is_deck_only(gap))
            .collect();
        assert_eq!(
            serde_json::to_string(&shared).unwrap(),
            serde_json::to_string(&row.gap_details).unwrap(),
            "{key}: every gap but the deck-only entry gaps equals the report byte for byte"
        );
        if shared.len() == card.gap_details.len() {
            assert!(!row.supported, "{key}: same gaps, so the report agrees");
        } else {
            deck_only_cards.push(card.name.as_str());
        }
        for gap in &card.gap_details {
            assert!(
                gap.source_text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty()),
                "{key}: {} publishes blank source text",
                gap.handler
            );
            families.insert(gap.handler.split(':').next().unwrap_or_default());
        }
        let face = db.get_face_by_name(&card.name).expect("pool face");
        if let (Some(class), Some(entry)) = (
            face.metadata.confirmed_misparse,
            ConfirmedMisparses::committed().get(&key),
        ) {
            census_cards += 1;
            let census = card
                .gap_details
                .iter()
                .find(|gap| gap.handler == class.handler())
                .unwrap_or_else(|| panic!("{key}: the census gap is published"));
            assert_eq!(
                census.source_text.as_deref(),
                Some(entry.clauses.join(" / ").as_str()),
                "{key}: the published text is the census clauses"
            );
        }
    }
    // The verdict never hides a gap the report publishes.
    for (key, row) in &report {
        if !row.supported {
            assert!(
                coverage
                    .unsupported_cards
                    .iter()
                    .any(|card| card.name.to_lowercase() == *key),
                "{key}: the report lists it unsupported, so the verdict must"
            );
        }
    }
    // Reach guards: every gap family the witnesses stand for, and the divergence.
    for family in ["Misparse", "Swallow", "SilentDrop", "ResolverFeature"] {
        assert!(
            families.contains(family),
            "no {family} gap reached: {families:?}"
        );
    }
    assert!(
        census_cards >= 5,
        "census witnesses reached: {census_cards}"
    );
    unsupported_named(&coverage, "Absolving Lammasu");
    assert!(deck_only_cards.contains(&"Arcane Adaptation"));
    for supported in ["Plains", "Lightning Bolt", "Metallic Mimic"] {
        assert!(
            !coverage
                .unsupported_cards
                .iter()
                .any(|card| card.name == supported),
            "{supported} is supported"
        );
    }
}

/// The wire addition keeps every old field and adds `gap_details` only when
/// there is something to say. The old fields are compared with the per-face
/// computation the verdict made before this field existed.
#[test]
fn unsupported_card_old_fields_are_unchanged_beside_gap_details() {
    let db = printed_pool(&["plains", "hinder"]);
    let coverage = verdict_coverage(&db, &["Plains", "Hinder", "Hinder"]);
    assert_eq!((coverage.total_unique, coverage.supported_unique), (2, 1));
    let card = unsupported_named(&coverage, "Hinder");
    let face = db.get_face_by_name("Hinder").expect("printed Hinder");
    let face_coverage = card_face_coverage_with_subtypes(face, db.valid_subtypes());

    assert_eq!(card.copies, 2);
    assert_eq!(card.gaps, face_coverage.gap_handlers());
    assert_eq!(card.oracle_text, face.oracle_text);
    assert_eq!(
        serde_json::to_value(&card.parse_details).unwrap(),
        serde_json::to_value(&face_coverage.parse_details).unwrap()
    );

    let wire = serde_json::to_value(card).unwrap();
    assert_eq!(wire["name"], "Hinder");
    assert_eq!(wire["copies"], 2);
    assert_eq!(
        wire["gaps"],
        serde_json::json!(["Swallow:Replacement_Instead"])
    );
    assert_eq!(
        wire["gap_details"][0]["handler"], "Swallow:Replacement_Instead",
        "the new field is on the wire: {wire}"
    );
    let round_trip: UnsupportedCard = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&round_trip).unwrap(), wire);

    // Sibling: without details the key is absent, so the old shape is unchanged.
    let mut bare = card.clone();
    bare.gap_details.clear();
    assert!(serde_json::to_value(&bare)
        .unwrap()
        .get("gap_details")
        .is_none());

    // A whole pre-change payload (no `gap_details` anywhere) still deserializes.
    let legacy: DeckCoverage = serde_json::from_value(serde_json::json!({
        "total_unique": 2,
        "supported_unique": 1,
        "unsupported_cards": [{ "name": "Hinder", "gaps": ["Swallow:Replacement_Instead"] }],
    }))
    .expect("a payload from before gap_details deserializes");
    assert!(legacy.unsupported_cards[0].gap_details.is_empty());
    assert_eq!(legacy.unsupported_cards[0].copies, 1);
}

/// The summary-only hint path never builds coverage, so it is unchanged by the
/// new field; the same request without `summary_only` does build it.
#[test]
fn deck_compatibility_summary_only_still_skips_coverage() {
    let db = printed_pool(&["plains", "hinder"]);
    let mut request = DeckCompatibilityRequest {
        main_deck: vec!["Plains".to_string(), "Hinder".to_string()],
        selected_format: Some(SelectedFormat::Tag(GameFormat::Vintage)),
        ..Default::default()
    };
    let full = evaluate_deck_compatibility(&db, &request);
    let full = full.coverage.expect("full verdict reports coverage");
    assert!(
        !full.unsupported_cards.is_empty(),
        "reach: the same request builds coverage when not summary-only"
    );
    request.summary_only = true;
    assert!(evaluate_deck_compatibility(&db, &request)
        .coverage
        .is_none());
}
