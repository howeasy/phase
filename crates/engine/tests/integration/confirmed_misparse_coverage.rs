//! Coverage metadata changes support classification, never parsed rules.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, card_face_coverage_with_subtypes, card_face_gaps, CardCoverageResult,
    GapDiagnosis,
};
use engine::game::deck_loading::default_scheme_deck_entries;
use engine::game::deck_validation::{
    evaluate_deck_compatibility, DeckCompatibilityRequest, UnsupportedCard,
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
    let key = "a good day to die";
    let entry = ConfirmedMisparses::committed()
        .get(key)
        .expect("the committed list carries this face");
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
        swallow_text.contains("top or bottom of its owner's library"),
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
