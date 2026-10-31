#!/usr/bin/env python3
"""Standalone folding migration contracts; deliberately not a Gate H dependency."""
from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/migrate-folding-parsed.py"
SPEC = importlib.util.spec_from_file_location("folding_migration", SCRIPT)
migration = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(migration)
rd = migration.rd
P = rd.SCOPE + "/probe.rs"
F = "parse_type_phrase_folding"
W = migration.WAIVER_PATH
COMPANION = f"\nfn companion(i: &str) {{ let (companion, _) = {F}(i); consume(companion); }}\n"


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / rd.SCOPE).mkdir(parents=True)
        shutil.copyfile(ROOT / "rust-toolchain.toml", self.root / "rust-toolchain.toml")

    def contract(self, before, expected, legacy=1, waivers=1, path=P):
        """J1 runs real snapshot classifiers, with independent debt expectations."""
        hits = rd.find_discards(path, before.splitlines())
        self.assertEqual(sum(h.symbol in rd.FOLDING_HELPERS for h in hits), legacy)
        self.assertGreater(legacy, 0, "tuple-era production reach guard")
        plan = migration.plan_source(path, before)
        self.assertFalse(plan.unsupported, plan.unsupported)
        self.assertEqual(plan.rendered, expected)
        self.assertGreater(plan.calls, 0)
        second = migration.plan_source(path, expected)
        self.assertFalse(second.unsupported, second.unsupported)
        self.assertEqual(second.rendered, expected)
        self.assertFalse(second.edits)
        for output in (expected, migration.format_source(self.root, path, expected)):
            origins = rd.parsed_returning_helpers([output])
            self.assertEqual(rd.find_disposition_violations(path, output, origins), [])
            after_hits = rd.find_discards(path, output.splitlines())
            self.assertEqual(sum(h.family == "unaudited-waiver" for h in after_hits), waivers)
            self.assertEqual(Counter(h.family for h in hits if h.symbol not in rd.FOLDING_HELPERS
                                     and h.family != "unaudited-waiver"),
                             Counter(h.family for h in after_hits if h.family != "unaudited-waiver"))
            formatted_again = migration.plan_source(path, output)
            self.assertFalse(formatted_again.unsupported, formatted_again.unsupported)
            self.assertFalse(formatted_again.edits)
        return plan

    def test_existing_dispositions_preserve_target_and_default_debt(self):
        before = (f"fn f() {{ let (v, rest) = {F}(i).with_rest(); "
                  f"let old = {F}(j).waive({W}); "
                  "let (target, _) = parse_target(i); let default = filter.unwrap_or(TargetFilter::Any); }"
                  + COMPANION)
        expected = before.replace(f"let (companion, _) = {F}(i);",
                                  f"let companion = {F}(i).waive({W});")
        self.contract(before, expected, waivers=2)

    def test_family_discard_and_retention(self):
        for helper in sorted(rd.FOLDING_HELPERS):
            for residue in ("_", "_unused"):
                with self.subTest(helper=helper, residue=residue):
                    self.contract(f"fn f() {{ let (mut value, {residue}) = {helper}(side_effect(i)); use_value(value); }}",
                                  f"fn f() {{ let mut value = {helper}(side_effect(i)).waive({W}); use_value(value); }}")
            before = f"fn f() {{ let (value, rest) = {helper}(i); consume(value, rest); }}" + COMPANION
            expected = before.replace(f"{helper}(i);", f"{helper}(i).with_rest();", 1).replace(
                f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
            self.contract(before, expected)

    def test_typed_wrapped_and_refutable_patterns(self):
        for wrapper in ("", "Some", "Ok"):
            wrap = lambda s: f"{wrapper}({s})" if wrapper else s
            for prefix, suffix in (("let ", ";"), ("let ", " else { return; };"),
                                   ("if let ", " { consume(value); }")):
                before = "fn f() { " + prefix + wrap("(value, _)") + " = " + wrap(F + "(i)") + suffix + " }"
                expected = "fn f() { " + prefix + wrap("value") + " = " + wrap(F + f"(i).waive({W})") + suffix + " }"
                self.contract(before, expected)
        self.contract(f"fn f() {{ let (value, _): (Filter, &str) = {F}(i); }}",
                      f"fn f() {{ let value: Filter = {F}(i).waive({W}); }}")

    def test_lazy_multiline_projections(self):
        before = f"fn f() {{ let value = cached.unwrap_or_else(|| {{ crate::parser::oracle_target::{F}(\n    text,\n).0 }}); }}"
        self.contract(before, before.replace(".0", f".waive({W})"))
        before = f"fn f() {{ let rest = ({F}(i)).1; consume(rest); }}" + COMPANION
        expected = before.replace(f"{F}(i)).1", f"{F}(i).with_rest()).1").replace(
            f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
        self.contract(before, expected)

    def test_whole_tuple_assertions_both_operands(self):
        for args in (f'{F}(i), (Filter::Any, "")', f'(Filter::Any, ""), {F}(i)'):
            before = f'fn f() {{ assert_eq!(\n{args},\n "message {{}}", i); }}' + COMPANION
            expected = before.replace(F + "(i)", F + "(i).with_rest()", 1).replace(
                f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
            self.contract(before, expected)

    def test_one_waiver_before_match(self):
        before = f"fn f() {{ match {F}(side_effect(i)) {{ (Filter::Any, _) => first(), (other, _tail) if accepts(&other) => second(other), }} }}"
        expected = f"fn f() {{ match {F}(side_effect(i)).waive({W}) {{ Filter::Any => first(), other if accepts(&other) => second(other), }} }}"
        self.refusal(before, "unsupported-scope _tail")
        before = before.replace("(other, _tail)", "(other, _)")
        self.contract(before, expected, legacy=2)
        before = f"fn f() {{ match {F}(i) {{ (Filter::Any, _) | (Filter::SelfRef, _) => first(), (other, _) => second(other), }} }}"
        expected = f"fn f() {{ match {F}(i).waive({W}) {{ Filter::Any | Filter::SelfRef => first(), other => second(other), }} }}"
        self.contract(before, expected, legacy=3)

    def test_context_and_mixed_branch_tails(self):
        for expr in (f"ctx.with_player_antecedents(|s| {F}(i))",
                     f"ctx.with_player_scope(scope, |s| {{ observe(); {F}(i) }})",
                     f"ctx.with_player_antecedents(|s| -> (Filter, &str) {{ {F}(i) }})",
                     f"match grammar {{ Legacy => {F}(i), Strict => {{ let (r, f) = strict(i)?; (f, r) }} }}",
                     f"if legacy {{ {F}(i) }} else {{ strict(i) }}"):
            before = f"fn f() {{ let (value, rest) = {expr}; consume(value, rest); }}" + COMPANION
            expected = before.replace(F + "(i)", F + "(i).with_rest()", 1).replace(
                f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
            self.contract(before, expected)
        before = f"fn f() {{ let (value, _) = ctx.with_player_antecedents(|s| -> (Filter, &str) {{ {F}(i) }}); }}"
        expected = f"fn f() {{ let value = ctx.with_player_antecedents(|s| -> Filter {{ {F}(i).waive({W}) }}); }}"
        self.contract(before, expected)

    def test_debug_observation_and_collision(self):
        for helper in (F, "parse_mass_type_union"):
            before = (f"fn f() {{\n    let parsed_value = already();\n    let (value, _rem) = {helper}(i);\n"
                      "    #[cfg(debug_assertions)]\n    assert_no_compound_remainder(_rem, i);\n    consume(value);\n}")
            expected = (f"fn f() {{\n    let parsed_value = already();\n    let parsed_value_2 = {helper}(i);\n"
                        "    #[cfg(debug_assertions)]\n    assert_no_compound_remainder(parsed_value_2.remainder(), i);\n"
                        f"    let value = parsed_value_2.waive({W});\n    consume(value);\n}}")
            self.contract(before, expected)
            self.assertNotIn("debug_assert!", expected)

    def test_enclosing_tuple_return_disposal_and_alias(self):
        for expr in (F + "(i)", "{ observe(); " + F + "(i) }", "ctx.with_player_antecedents(|s| " + F + "(i))"):
            before = f"fn f(i: &str) -> (Filter, &str) {{ {expr} }}" + COMPANION
            expected = before.replace(F + "(i)", F + "(i).with_rest()", 1).replace(
                f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
            self.contract(before, expected)
        for expr in (f"drop({F}(i));", f"let _ = {F}(i);", f"{F}(i);"):
            self.contract("fn f() { " + expr + " }", "fn f() { " + expr.replace(F + "(i)", F + f"(i).waive({W})") + " }")
        before = f"use crate::parser::oracle_target::{{{F} as folding}};\nfn f() {{ let (v, _) = folding(i); }}"
        self.contract(before, before.replace("let (v, _) = folding(i)", f"let v = folding(i).waive({W})"))

    def test_lexical_controls_and_test_scope(self):
        literal = 'let text = "é fake parse_type_phrase_folding(i).0 \\\n  more parse_mass_type_union(i)"; /* nested /* helper */ end */ '
        before = "fn f() { " + literal + f"let (v, _) = {F}(i); }}"
        expected = before.replace(f"let (v, _) = {F}(i)", f"let v = {F}(i).waive({W})")
        self.contract(before, expected)
        before = f"#[cfg(test)] mod tests {{ fn f() {{ let (v, _) = {F}(i); }} }}" + COMPANION
        expected = before.replace(f"let (v, _) = {F}(i)", f"let v = {F}(i).waive({W})").replace(
            f"let (companion, _) = {F}(i);", f"let companion = {F}(i).waive({W});")
        self.contract(before, expected)
        self.assertNotIn("use ", expected)

    def test_pure_tree_planning_and_overlap_refusal(self):
        before = {P: f"fn f() {{ let (v, _) = {F}(i); }}"}
        expected = {P: f"fn f() {{ let v = {F}(i).waive({W}); }}"}
        plan = migration.plan_tree(before)
        self.assertFalse(plan.unsupported, plan.unsupported)
        self.assertEqual(plan.rendered, expected)
        self.assertEqual(before[P], f"fn f() {{ let (v, _) = {F}(i); }}")
        with self.assertRaisesRegex(ValueError, "overlap"):
            migration.render_edits("abcd", (migration.Edit(0, 3, "x", "probe"),
                                             migration.Edit(2, 4, "y", "probe")))
        self.contract(before[P], expected[P])

    def test_f2_format_capture_refuses_without_writes(self):
        for macro in ('println!("{_rest}")', 'format_args!("{_rest:?}")',
                      'write!(out, "{_rest:>w$}")', 'assert!(ok, "{_rest}")'):
            with self.subTest(macro=macro):
                before = f"fn f() {{ let (value, _rest) = {F}(i); {macro}; consume(value); }}"
                dest = self.root / P
                dest.write_text(before)
                result = subprocess.run([sys.executable, "-B", str(SCRIPT),
                                         "--apply", "--root", str(self.root)],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("unsupported implicit format-capture remainder read", result.stderr)
                self.assertEqual(dest.read_text(), before)
        before = f'fn f() {{ let (value, _rest) = {F}(i); println!("{{{{_rest}}}}"); consume(value); }}'
        self.contract(before, before.replace(f"let (value, _rest) = {F}(i)",
                                            f"let value = {F}(i).waive({W})"))

    def refusal(self, source, category):
        plan = migration.plan_source(P, source)
        self.assertTrue(any(category in error for error in plan.unsupported), plan.unsupported)
        self.assertEqual((plan.rendered, plan.edits), (source, ()))

    def test_oa_debug_only_shapes_refuse_instead_of_losing_debt(self):
        for observation in ("#[cfg(debug_assertions)] check_empty(_rem, i);",
                            "#[cfg(debug_assertions)] { assert!(_rem.is_empty()); }",
                            "debug_assert!(_rem.is_empty());",
                            "consume(value); #[cfg(debug_assertions)] assert_no_compound_remainder(_rem, i);"):
            with self.subTest(observation=observation):
                before = f"fn f() {{ let (value, _rem) = {F}(i); {observation} consume(value); }}"
                plan = migration.plan_source(P, before)
                category = ("unsupported-scope _rem" if observation == "debug_assert!(_rem.is_empty());"
                            else "unsupported debug-only remainder observation")
                self.assertTrue(any(category in error for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
        # The registered immediate observation remains supported.
        self.test_debug_observation_and_collision()

    def test_oa_parameter_bindings_refuse_ambiguous_scope(self):
        for body in ("let g = |_rest: &str| _rest.len();",
                     "let g = |_rest: &str| { _rest.len() };",
                     "for _rest in tails { consume(_rest); }",
                     "match other { Some(_rest) => consume(_rest), None => (), }"):
            with self.subTest(body=body):
                before = f"fn f() {{ let (value, _rest) = {F}(i); {body} consume(value); }}"
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported-scope _rest" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
        before = f"fn f() {{ let (value, _rest) = {F}(i); consume(_rest); }}"
        self.contract(before, before.replace("(value, _rest)", "(value, rest)").replace(
            F + "(i)", F + "(i).with_rest()").replace("consume(_rest)", "consume(rest)"), waivers=0)

    def test_ob_unrelated_typed_closure_refuses(self):
        for body in (f"let (v, _) = {F}(x); (v, 1)", f"({F}(x).0, 1)"):
            with self.subTest(body=body):
                before = f"fn f() {{ let g = |x: &str| -> (Filter, usize) {{ {body} }}; }}"
                plan = migration.plan_source(P, before)
                self.assertTrue(plan.unsupported)
                self.assertEqual(plan.rendered, before)
        before = f"fn f() {{ let (v, _) = ctx.with_player_antecedents(|x| -> (Filter, &str) {{ {F}(x) }}); }}"
        expected = f"fn f() {{ let v = ctx.with_player_antecedents(|x| -> Filter {{ {F}(x).waive({W}) }}); }}"
        self.contract(before, expected)

    def test_shadowed_underscore_plain_let_rewrites_independently(self):
        before = (f"fn f() {{ let (value, _rest) = {F}(i); "
                  f"let (colored, _rest) = {F}(j); consume(value, colored); }}")
        expected = (f"fn f() {{ let value = {F}(i).waive({W}); "
                    f"let colored = {F}(j).waive({W}); consume(value, colored); }}")
        self.contract(before, expected, legacy=2, waivers=2)

    def test_used_underscore_remainder_retains_debug_and_runtime_reads(self):
        before = (
            "fn f() {\n"
            "    let (parsed_target, _rem) = parse_mass_type_union(rest, ctx);\n"
            "    #[cfg(debug_assertions)]\n"
            "    assert_no_compound_remainder(_rem, text);\n"
            "    let target = if filter_targets_stack(&parsed_target) {\n"
            "        constrain_filter_to_stack(parsed_target)\n"
            "    } else { parsed_target };\n"
            "    let (origin, target) = compose_exile_graveyard_origin_and_target(\n"
            "        origin, target, rest, _rem, ctx, EffectScope::All,\n"
            "    );\n"
            "    consume(origin, target);\n"
            "}")
        expected = (
            "fn f() {\n"
            "    let (parsed_target, rem) = parse_mass_type_union(rest, ctx).with_rest();\n"
            "    #[cfg(debug_assertions)]\n"
            "    assert_no_compound_remainder(rem, text);\n"
            "    let target = if filter_targets_stack(&parsed_target) {\n"
            "        constrain_filter_to_stack(parsed_target)\n"
            "    } else { parsed_target };\n"
            "    let (origin, target) = compose_exile_graveyard_origin_and_target(\n"
            "        origin, target, rest, rem, ctx, EffectScope::All,\n"
            "    );\n"
            "    consume(origin, target);\n"
            "}")
        self.contract(before, expected, waivers=0)
        self.assertNotIn(".waive(", expected)
        self.assertNotIn(".remainder()", expected)

    def test_used_underscore_remainder_rebinding_refuses(self):
        before = (f"fn f() {{ let (mut value, _tail) = {F}(i); "
                  "consume(value, _tail); { let _tail = other(); consume(_tail); } "
                  f"let (other, _tail) = {F}(j); consume(other); }}")
        plan = migration.plan_source(P, before)
        self.assertTrue(any("unsupported-scope _tail" in error
                            for error in plan.unsupported), plan.unsupported)
        self.assertEqual(plan.rendered, before)
        self.contract(f"fn f() {{ let (v, _unused) = {F}(i); consume(v); }}",
                      f"fn f() {{ let v = {F}(i).waive({W}); consume(v); }}")

    def test_used_underscore_remainder_name_collision_refuses(self):
        for prefix, pattern, body, category in (
                ("let tail = previous(); ", "(value, _tail)", "consume(value, _tail, tail);", "collision"),
                ("", "(tail, _tail)", "consume(tail, _tail);", "collision"),
                ("", "(value, _tail)", "let tail = later(); consume(value, _tail, tail);", "collision"),
                ("", "(value, _tail)", "{ let tail = nested(); consume(_tail, tail); }",
                 "unsupported-scope _tail")):
            before = f"fn f() {{ {prefix}let {pattern} = {F}(i); {body} }}"
            with self.subTest(before=before):
                plan = migration.plan_source(P, before)
                self.assertTrue(plan.unsupported)
                self.assertEqual(plan.rendered, before)
                self.assertTrue(any(category in error for error in plan.unsupported))
        for source in (
                f"fn f(tail: &str) {{ let (value, _tail) = {F}(i); consume(_tail); }}",
                f"fn f() {{ let tail = outer(); if ready() {{ let (value, _tail) = {F}(i); consume(_tail); }} }}",
                f"fn f() {{ ctx.with_player_antecedents(|tail| {{ let (value, _tail) = {F}(i); consume(_tail); }}); }}",
                f"fn f() {{ for tail in tails {{ let (value, _tail) = {F}(i); consume(_tail); }} }}",
                f"fn f() {{ match input {{ Some(tail) => {{ let (value, _tail) = {F}(i); consume(_tail); }}, _ => (), }} }}"):
            with self.subTest(source=source):
                plan = migration.plan_source(P, source)
                self.assertTrue(plan.unsupported)
                self.assertEqual(plan.rendered, source)
                self.assertTrue(any("collision" in error for error in plan.unsupported))

    def test_d1_enclosing_function_rebindings_refuse_for_let_and_match(self):
        bindings = (
            "let Some(_rest) = opt else { return; };",
            "if let Some(_rest) = opt { use_it(_rest); }",
            "while let Some(_rest) = next() { use_it(_rest); }",
            "for _rest in tails { use_it(_rest); }",
            "let n = apply(|_rest: &str| _rest.len()) + _rest.len();",
            "let g = match x { _ => |_rest: &str| _rest.len() };",
            "let s = S { g: |_rest: &str| _rest.len() };",
            "match opt { Some(_rest) => use_it(_rest), None => (), }",
            "fn inner(_rest: &str) { use_it(_rest); }",
            "fn _rest() {}",
            "const _rest: usize = 1;",
        )
        for binding in bindings:
            for form in ("let", "match"):
                with self.subTest(binding=binding, form=form):
                    if form == "let":
                        before = (f"fn f() {{ let (v, _rest) = {F}(i); "
                                  + binding + " consume(v); }")
                    else:
                        before = (f"fn f() {{ match {F}(i) {{ (v, _rest) => {{ "
                                  + binding + " consume(v); } } }")
                    plan = migration.plan_source(P, before)
                    self.assertTrue(any("unsupported-scope _rest" in error
                                        for error in plan.unsupported), plan.unsupported)
                    self.assertEqual(plan.rendered, before)
                    self.assertFalse(plan.edits)
                    control = before.replace(binding, binding.replace("_rest", "other_rest"))
                    if form == "match":
                        self.refusal(control, "unsupported-scope _rest")
                        control = control.replace("(v, _rest)", "(v, _)")
                    self.assertFalse(migration.plan_source(P, control).unsupported)
        before = f"fn f(_rest: &str) {{ let (v, _rest) = {F}(i); consume(v); }}"
        self.assertTrue(any("unsupported-scope _rest" in error
                            for error in migration.plan_source(P, before).unsupported))

    def test_d1_match_arm_implicit_captures_refuse(self):
        for literal in ("{_rest}", "{_rest:?}", "{_rest:>w$}"):
            with self.subTest(literal=literal):
                before = (f'fn f() {{ match {F}(i) {{ (v, _rest) => {{ '
                          f'println!("{literal}"); consume(v); }} }} }}')
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported implicit format-capture remainder read" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
        before = f'fn f() {{ match {F}(i) {{ (v, _rest) => {{ println!("{{{{_rest}}}}"); consume(v); }} }} }}'
        self.refusal(before, "unsupported-scope _rest")
        before = before.replace("(v, _rest)", "(v, _)")
        expected = before.replace(F + "(i)", F + f"(i).waive({W})").replace("(v, _)", "v")
        self.contract(before, expected)
        # A format capture before the binder cannot read this binding.
        before = f'fn f() {{ println!("{{_rest}}"); {{ let (v, _rest) = {F}(i); }} }}'
        self.contract(before, before.replace(f"let (v, _rest) = {F}(i)",
                                            f"let v = {F}(i).waive({W})"))
        control = f'fn unrelated() {{ println!("{{_rest}}"); }} fn f() {{ let (v, _rest) = {F}(i); }}'
        self.assertFalse(migration.plan_source(P, control).unsupported)

    def test_d1b_same_block_shadowing_limits_reads_and_keeps_initializer(self):
        before = (f"fn f() {{ let (v, _rest) = {F}(i); consume(v, _rest); "
                  f"let (other, _rest) = {F}(j); consume(other); }}")
        expected = (f"fn f() {{ let (v, rest) = {F}(i).with_rest(); consume(v, rest); "
                    f"let other = {F}(j).waive({W}); consume(other); }}")
        self.contract(before, expected, legacy=2, waivers=1)
        before = (f"fn f() {{ let (v, _rest) = {F}(i); "
                  "let _rest = inspect(_rest); consume(v, _rest); }")
        expected = (f"fn f() {{ let (v, rest) = {F}(i).with_rest(); "
                    "let _rest = inspect(rest); consume(v, _rest); }")
        self.contract(before, expected, waivers=0)
        before = (f"fn f() {{ let (v, _rest) = {F}(i); "
                  'let _rest = other(); println!("{_rest}"); consume(v); }')
        self.contract(before, before.replace(f"let (v, _rest) = {F}(i)",
                                            f"let v = {F}(i).waive({W})"))
        captured_initializer = before.replace("other()", 'format!("{_rest}")')
        plan = migration.plan_source(P, captured_initializer)
        self.assertTrue(any("unsupported implicit format-capture remainder read" in error
                            for error in plan.unsupported), plan.unsupported)
        self.assertEqual(plan.rendered, captured_initializer)

    def test_d1b_sibling_ancestor_and_earlier_bindings_are_independent(self):
        before = (f"fn f() {{ if a {{ let (v, _rest) = {F}(i); consume(v); }} "
                  f"if b {{ let (other, _rest) = {F}(j); consume(other); }} }}")
        expected = (f"fn f() {{ if a {{ let v = {F}(i).waive({W}); consume(v); }} "
                    f"if b {{ let other = {F}(j).waive({W}); consume(other); }} }}")
        self.contract(before, expected, legacy=2, waivers=2)
        for prefix, suffix in (
                ("let _rest = outside(); ", "consume(_rest);"),
                ("{ let _rest = earlier(); consume(_rest); } ", ""),
                ('if other { let _rest = sibling(); println!("{_rest}"); } ', "")):
            before = (f"fn f() {{ {prefix}if ready {{ let (v, _rest) = {F}(i); "
                      f"consume(v); }} {suffix} }}")
            expected = before.replace(f"let (v, _rest) = {F}(i)",
                                      f"let v = {F}(i).waive({W})")
            with self.subTest(prefix=prefix):
                self.contract(before, expected)

    def test_d1b_later_nested_binding_refuses_but_same_block_is_legal(self):
        for nested in ("{ let _rest = other(); consume(_rest); }",
                       "if ready { let _rest = other(); consume(_rest); }"):
            before = f"fn f() {{ let (v, _rest) = {F}(i); {nested} consume(v); }}"
            with self.subTest(nested=nested):
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported-scope _rest" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
                control = before.replace(nested, "let _rest = other(); consume(_rest);")
                self.assertFalse(migration.plan_source(P, control).unsupported)

    def test_d1b_real_imperative_and_dispatch_sibling_shapes(self):
        before = (
            "fn f() { if all { "
            "let (parsed_target, _rem) = parse_mass_type_union(rest, ctx); "
            "#[cfg(debug_assertions)] assert_no_compound_remainder(_rem, text); "
            "let target = constrain(parsed_target); "
            "let (origin, target) = compose(origin, target, rest, _rem, ctx); "
            "return result(origin, target); } "
            "if mass_zone { let (target, _rem) = "
            "ctx.with_player_antecedents(|scoped| parse_target_with_ctx(rest_text, scoped)); "
            "#[cfg(debug_assertions)] assert_no_compound_remainder(_rem, text); "
            "let (origin, target) = compose(origin, target, rest_text, _rem, ctx); "
            "return result(origin, target); } }")
        expected = before.replace(
            "let (parsed_target, _rem) = parse_mass_type_union(rest, ctx)",
            "let (parsed_target, rem) = parse_mass_type_union(rest, ctx).with_rest()").replace(
                "assert_no_compound_remainder(_rem, text)", "assert_no_compound_remainder(rem, text)", 1
            ).replace("compose(origin, target, rest, _rem, ctx)", "compose(origin, target, rest, rem, ctx)")
        self.contract(before, expected, waivers=0)
        before = (
            f"fn f() {{ if let Some((subject, _)) = read(i) {{ let (affected, _rest) = {F}(&subject); "
            "return result(affected); } "
            "if let Some((prefix, part, amount)) = read_cost(i) { "
            f'let text = format!("{{prefix}}{{part}}"); let (affected, _rest) = {F}(&text); '
            "return result(affected, amount); } }")
        expected = before.replace(f"let (affected, _rest) = {F}(&subject)",
                                  f"let affected = {F}(&subject).waive({W})").replace(
                                      f"let (affected, _rest) = {F}(&text)",
                                      f"let affected = {F}(&text).waive({W})")
        self.contract(before, expected, legacy=2, waivers=2)

    def test_same_block_rebinding_survives_unrelated_statement_syntax(self):
        before = (
            "fn f() { use crate::types::{Filter, Kind}; "
            f"let (first, _rest) = {F}(i); "
            "let Some(value) = opt else { return; }; "
            "let project = |x| S { x }; "
            "macro_rules! helper { ($q:expr) => { $q } } "
            "assert!(project(value).ready()); "
            f"let (second, _rest) = {F}(j); consume(first, second); }}")
        expected = before.replace(f"let (first, _rest) = {F}(i)",
                                  f"let first = {F}(i).waive({W})").replace(
                                      f"let (second, _rest) = {F}(j)",
                                      f"let second = {F}(j).waive({W})")
        self.contract(before, expected, legacy=2, waivers=2)

    def test_d1c_match_visibility_stops_at_plain_let_and_arm_boundary(self):
        for suffix in ('let _rest = other(); println!("{_rest}");',
                       'let _rest = other(); consume(_rest);'):
            before = (f"fn f() {{ match {F}(i) {{ (v, _rest) => {{ {suffix} consume(v); }} }} "
                      'println!("{_rest}"); }')
            with self.subTest(suffix=suffix):
                self.refusal(before, "unsupported-scope _rest")
        before = (f"fn f() {{ let _rest = outer(); match {F}(i) {{ (v, _rest) => {{ consume(v); }} }} "
                  'println!("{_rest}"); }')
        self.refusal(before, "unsupported-scope _rest")
        for statement, category in (
                ("let _rest = inspect(_rest);", "unsupported-scope _rest"),
                ('let _rest = format!("{_rest}");', "unsupported implicit format-capture remainder read"),
                ("{ let _rest = other(); }", "unsupported-scope _rest")):
            before = f"fn f() {{ match {F}(i) {{ (v, _rest) => {{ {statement} consume(v); }} }} }}"
            with self.subTest(statement=statement):
                plan = migration.plan_source(P, before)
                self.assertTrue(any(category in error for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)

    def test_d2_configuration_only_reads_refuse(self):
        for observation in (
                "#[cfg(any(debug_assertions, test))] check_empty(_rem, i);",
                "#[cfg(all(debug_assertions))] { check_empty(_rem, i); }",
                "#[cfg(test)] check_empty(_rem, i);",
                "if cfg!(debug_assertions) { check_empty(_rem, i); }",
                "#[cfg_attr(feature = \"checks\", cfg(test))] check_empty(_rem, i);",
                "if !cfg!(test) { check_empty(_rem, i); } else { use_it(_rem); }"):
            with self.subTest(observation=observation):
                before = f"fn f() {{ let (v, _rem) = {F}(i); {observation} consume(v); }}"
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported debug-only remainder observation" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
                # Adding a runtime read does not legalize a forbidden cfg read.
                control = before[:-1] + "consume(_rem); }"
                self.refusal(control, "unsupported debug-only remainder observation")

    def test_d3_other_typed_wrapper_dispositions_refuse(self):
        expression = f"ctx.with_player_antecedents(|c| -> (Filter, &str) {{ {F}(i) }})"
        for statement in (
                f"match {expression} {{ (v, _) => consume(v), }}",
                f"drop({expression});",
                f"{expression};",
                f"let (v, _rem) = {expression}; "
                "#[cfg(debug_assertions)] assert_no_compound_remainder(_rem, i); consume(v);"):
            with self.subTest(statement=statement):
                before = "fn f() { " + statement + " }"
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported typed wrapper disposition" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
                self.assertFalse(plan.edits)
                control = before.replace("|c| -> (Filter, &str)", "|c|")
                self.assertFalse(migration.plan_source(P, control).unsupported)

    def test_b1_nonlocal_name_positions_refuse_without_renaming(self):
        for expression in ("stringify!(_rest)", "concat!(_rest)", "concat_idents!(_rest)",
                           "unknown!(_rest)", "object._rest", "module::_rest"):
            before = f"fn f() {{ let (v, _rest) = {F}(i); consume(v, {expression}); }}"
            with self.subTest(expression=expression):
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported-scope _rest" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
                self.assertFalse(plan.edits)
                control = before.replace(expression, "inspect(_rest)")
                self.refusal(control, "unsupported-scope _rest")
                legal = before.replace(expression, "_rest")
                retained = migration.plan_source(P, legal)
                self.assertFalse(retained.unsupported, retained.unsupported)
                self.assertIn("consume(v, rest)", retained.rendered)
                self.assertIn(".with_rest()", retained.rendered)

    def test_b2_positive_positions_refuse_patterns_and_macro_tokens(self):
        for statement in (
                "xs.iter().map(|S { name: _rest, .. }| _rest.len());",
                "for S { x: _rest } in xs { consume(_rest); }",
                "let ok = matches!(y, Some(_rest) if _rest.is_empty());",
                "macro_rules! m { ($_rest:expr) => { $_rest.len() } }"):
            for form in ("let", "match"):
                before = (f"fn f() {{ let (v, _rest) = {F}(i); {statement} consume(v); }}"
                          if form == "let" else
                          f"fn f() {{ match {F}(i) {{ (v, _rest) => {{ {statement} consume(v); }} }} }}")
                with self.subTest(statement=statement, form=form):
                    plan = migration.plan_source(P, before)
                    self.assertTrue(any("unsupported-scope _rest" in error
                                        for error in plan.unsupported), plan.unsupported)
                    self.assertEqual(plan.rendered, before)
                    self.assertFalse(plan.edits)
                    control = before.replace(statement, statement.replace("_rest", "other"))
                    if form == "match":
                        self.refusal(control, "unsupported-scope _rest")
                        control = control.replace("(v, _rest)", "(v, _)")
                    self.assertFalse(migration.plan_source(P, control).unsupported)
        before = f"fn f() {{ let (v, _rest) = {F}(i); consume(v, _rest); }}"
        self.contract(before, before.replace("(v, _rest)", "(v, rest)").replace(
            F + "(i)", F + "(i).with_rest()").replace("consume(v, _rest)", "consume(v, rest)"),
            waivers=0)

    def test_b3_cfg_expression_chains_are_not_runtime_retention(self):
        for statement in (
                "let _ = cfg!(debug_assertions) && check(_rem, i);",
                "cfg!(debug_assertions).then(|| check(_rem, i));",
                "let ok = !cfg!(debug_assertions) || check(_rem, i);",
                "let ok = cfg!(test) || check(_rem, i);"):
            before = f"fn f() {{ let (v, _rem) = {F}(i); {statement} consume(v); }}"
            with self.subTest(statement=statement):
                plan = migration.plan_source(P, before)
                self.assertTrue(any("unsupported debug-only remainder observation" in error
                                    for error in plan.unsupported), plan.unsupported)
                self.assertEqual(plan.rendered, before)
                self.assertFalse(plan.edits)
                control = before[:-1] + "consume(_rem); }"
                self.refusal(control, "unsupported debug-only remainder observation")

    def test_refuse_discard_flow_with_unknown_tuple_branch(self):
        for expression in (f"if legacy {{ {F}(i) }} else {{ strict(i) }}",
                           f"match grammar {{ Legacy => {F}(i), Strict => strict(i) }}"):
            for source in (f"fn f() {{ let (v, _) = {expression}; }}",
                           f"fn f() {{ let v = ({expression}).0; }}"):
                with self.subTest(source=source):
                    plan = migration.plan_source(P, source)
                    self.assertTrue(plan.unsupported)
                    self.assertEqual(plan.rendered, source)

    def test_refuse_unknown_flows_patterns_and_references(self):
        cases = (f"fn f() {{ macro_capture!({F}(i)); }}",
                 f"fn f() {{ let alias = {F}; }}",
                 f"fn f() {{ let (ref v, _) = {F}(i); }}",
                 f"fn f() {{ let (v, _tail) = {F}(i); consume(_tail); let tail = other(); }}",
                 f"fn f() {{ consume({F}(i)); }}",
                 f"fn f() {{ let pair = {F}(i); consume(pair.0); }}",
                 f"fn f() {{ match {F}(i) {{ (v, rest) => consume(rest), (_, _) => (), }} }}",
                 f"fn f() {{ let {F} = other; let (v, _) = {F}(i); }}",
                 f"fn f() {{ {F}(i).waive(reason); }}",
                 f"fn f() {{ {F}(i).with_rest().0; }}",
                 f"fn f() {{ let (v, _) = {F}(i); ")
        for source in cases:
            with self.subTest(source=source):
                plan = migration.plan_source(P, source)
                self.assertTrue(plan.unsupported)
                self.assertEqual(plan.rendered, source)
                self.assertRegex(plan.unsupported[0], r"probe.rs:\d+:\d+:")


class TreeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / rd.SCOPE).mkdir(parents=True)
        shutil.copyfile(ROOT / "rust-toolchain.toml", self.root / "rust-toolchain.toml")

    def put(self, path, source):
        dest = self.root / path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(source.encode())
        return dest

    def cli(self, mode):
        return subprocess.run([sys.executable, "-B", str(SCRIPT),
                               mode, "--root", str(self.root)], capture_output=True, text=True)

    def joint(self, original, rendered, legacy, waivers):
        before = rd.production_sources(self.root, False, original)
        self.assertEqual(sum(h.symbol in rd.FOLDING_HELPERS for p, s in before.items()
                             for h in rd.find_discards(p, s.splitlines())), legacy)
        self.assertGreater(legacy, 0)
        origins = rd.parsed_returning_helpers(rendered.values())
        for p, s in rendered.items():
            self.assertEqual(rd.find_disposition_violations(p, s, origins), [])
        prod = rd.production_sources(self.root, False, rendered)
        self.assertEqual(sum(h.family == "unaudited-waiver" for p, s in prod.items()
                             for h in rd.find_discards(p, s.splitlines())), waivers)

    def test_apply_replay_and_second_run(self):
        source = f"fn f(i: &str) {{ let (v, _) = {F}(i); consume(v); }}\n"
        dest = self.put(P, source)
        check = self.cli("--check")
        self.assertEqual(check.returncode, 1, check.stderr)
        self.assertIn("classified_calls=1", check.stderr)
        replay = self.cli("--replay")
        self.assertEqual(replay.returncode, 0, replay.stderr)
        self.assertEqual(dest.read_text(), source)
        applied_patch = subprocess.run(["git", "apply", "-"], input=replay.stdout,
                                       cwd=self.root, capture_output=True, text=True)
        self.assertEqual(applied_patch.returncode, 0, applied_patch.stderr)
        replay_bytes = dest.read_bytes()
        self.joint({P: source}, {P: replay_bytes.decode()}, 1, 1)
        dest.write_bytes(source.encode())
        apply = self.cli("--apply")
        self.assertEqual(apply.returncode, 0, apply.stderr)
        self.assertEqual(dest.read_bytes(), replay_bytes)
        self.joint({P: source}, {P: dest.read_text()}, 1, 1)
        self.assertEqual(self.cli("--apply").returncode, 0)
        self.assertEqual(dest.read_bytes(), replay_bytes)
        self.assertEqual(self.cli("--check").returncode, 0)
        replay2 = self.cli("--replay")
        self.assertEqual((replay2.returncode, replay2.stdout), (0, ""))

    def test_replay_without_original_final_newline(self):
        original = f"fn f() {{ let (v, _) = {F}(i); }}"
        dest = self.put(P, original)
        replay = self.cli("--replay")
        self.assertEqual(replay.returncode, 0, replay.stderr)
        applied = subprocess.run(["git", "apply", "-"], input=replay.stdout, cwd=self.root,
                                 capture_output=True, text=True)
        self.assertEqual(applied.returncode, 0, applied.stderr)
        self.joint({P: original}, {P: dest.read_text()}, 1, 1)
        self.assertEqual(self.cli("--check").returncode, 0)

    def test_debug_observations_in_separate_files(self):
        for path, helper in ((P, F), (rd.SCOPE + "/mass.rs", "parse_mass_type_union")):
            self.put(path, f"fn f() {{ let (value, _rem) = {helper}(i);\n"
                     "#[cfg(debug_assertions)]\nassert_no_compound_remainder(_rem, i);\n"
                     "consume(value); }\n")
        plan = migration.prepare_tree(self.root)
        self.assertFalse(plan.unsupported, plan.unsupported)
        self.assertEqual(len(plan.changed), 2)
        self.joint(plan.originals, plan.rendered, 2, 2)
        for source in plan.rendered.values():
            self.assertIn("#[cfg(debug_assertions)]", source)
            self.assertLess(source.index(".remainder()"), source.index(".waive("))
            self.assertNotIn("debug_assert!", source)

    def test_complete_synthetic_cutover_fixpoint(self):
        producer = (
            f"fn {F}(i: &str) -> Parsed<'_, Filter> {{ parse_type_phrase_folding_with_ctx(i) }}\n"
            "fn parse_type_phrase_folding_with_ctx(i: &str) -> Parsed<'_, Filter> { Parsed::new(Filter::Any, i) }\n"
            "fn read_type_union_leg(i: &str) -> Parsed<'_, Filter> { match grammar { "
            "Legacy => parse_type_phrase_folding(i), Strict => { let (value, rest) = parse_target(i); "
            "Parsed::new(value, rest) } } }\n"
            "fn fold_type_union(i: &str) -> Parsed<'_, Filter> { "
            "let (value, rest) = read_type_union_leg(i).with_rest(); Parsed::new(value, rest) }\n"
            "fn parse_filter_phrase_union(i: &str) -> Parsed<'_, Filter> { fold_type_union(i) }\n"
            "fn parse_mass_type_union(i: &str) -> Parsed<'_, Filter> { fold_type_union(i) }\n")
        manual = migration.plan_source(P, producer)
        self.assertFalse(manual.unsupported, manual.unsupported)
        self.assertEqual(manual.rendered, producer)
        self.assertEqual(manual.counts["definition"], 6)
        types = ("pub struct Parsed<'a, T> { value: T, rest: &'a str }\n"
                 "pub enum ResidueWaiver { Unaudited }\n"
                 "impl<'a, T> Parsed<'a, T> { fn with_rest(self) -> (T, &'a str) { (self.value, self.rest) } }\n")
        self.put(rd.SCOPE + "/parsed.rs", types)
        self.put(rd.SCOPE + "/mod.rs", "mod parsed; mod probe;\n")
        old = producer + f"fn f() {{ let (v, _) = {F}(i); consume(v); }}\n"
        self.put(P, old)
        result = migration.prepare_tree(self.root)
        self.assertFalse(result.unsupported, result.unsupported)
        self.assertTrue(result.changed)
        self.assertIn(producer.splitlines()[0].split("{")[0].strip(), result.rendered[P])
        self.joint(result.originals, result.rendered, 1, 1)
        for path, source in result.rendered.items():
            self.put(path, source)
        second = migration.prepare_tree(self.root)
        self.assertFalse(second.unsupported, second.unsupported)
        self.assertFalse(second.changed)
        self.joint(result.originals, second.rendered, 1, 1)

    def test_whole_tree_refusal_in_both_orders(self):
        for bad, good in (("a.rs", "z.rs"), ("z.rs", "a.rs")):
            self.put(rd.SCOPE + "/" + good, f"fn f() {{ let (v, _) = {F}(i); }}")
            self.put(rd.SCOPE + "/" + bad, f"fn f() {{ capture!({F}(i)); }}")
            originals = {p: p.read_bytes() for p in self.root.rglob("*.rs")}
            result = self.cli("--apply")
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(originals, {p: p.read_bytes() for p in originals})

    def test_d1_d2_d3_refusal_preflights_whole_tree(self):
        cases = (
            (f"let (v, _rest) = {F}(i); if let Some(_rest) = opt {{ use_it(_rest); }}",
             "unsupported-scope _rest"),
            (f'match {F}(i) {{ (v, _rest) => {{ println!("{{_rest}}"); consume(v); }} }}',
             "unsupported implicit format-capture remainder read"),
            (f"let (v, _rem) = {F}(i); #[cfg(test)] check_empty(_rem, i);",
             "unsupported debug-only remainder observation"),
            (f"drop(ctx.with_player_antecedents(|c| -> (Filter, &str) {{ {F}(i) }}));",
             "unsupported typed wrapper disposition"),
            (f"let (v, _rest) = {F}(i); consume(v, stringify!(_rest));",
             "unsupported-scope _rest"),
            (f"let (v, _rest) = {F}(i); consume(v, object._rest);",
             "unsupported-scope _rest"),
            (f"let (v, _rest) = {F}(i); for S {{ x: _rest }} in xs {{}}",
             "unsupported-scope _rest"),
            (f"let (v, _rem) = {F}(i); let _ = cfg!(test) && check(_rem, i);",
             "unsupported debug-only remainder observation"),
        )
        for bad, category in cases:
            for first, second in (("a.rs", "z.rs"), ("z.rs", "a.rs")):
                with self.subTest(category=category, first=first):
                    good_path = self.put(rd.SCOPE + "/" + first,
                                         f"fn good() {{ let (v, _) = {F}(i); }}")
                    bad_path = self.put(rd.SCOPE + "/" + second, "fn bad() { " + bad + " }")
                    originals = {p: p.read_bytes() for p in (good_path, bad_path)}
                    result = self.cli("--apply")
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn(category, result.stderr)
                    self.assertEqual(originals, {p: p.read_bytes() for p in originals})

    def test_external_string_continuation_and_executable_reach(self):
        self.put(P, f"fn f() {{ let (v, _) = {F}(i); }}")
        outside = "crates/engine/tests/integration/continued.rs"
        literal = 'fn f() { let text = "fake parse_type_phrase_folding(i) \\\n  parse_mass_type_union(i)"; }'
        external = self.put(outside, literal)
        self.assertEqual(self.cli("--check").returncode, 1)
        success = migration.prepare_tree(self.root)
        self.assertFalse(success.unsupported, success.unsupported)
        self.joint(success.originals, success.rendered, 1, 1)
        external.write_text(literal[:-1] + f" let (v, _) = {F}(i); }}")
        result = self.cli("--apply")
        self.assertEqual(result.returncode, 2)
        self.assertIn(outside, result.stderr)
        self.assertIn("external", result.stderr)

    def test_formatter_failure_changed_input_and_install_rollback(self):
        dest = self.put(P, f"fn f() {{ let (v, _) = {F}(i); }}")
        source = dest.read_bytes()
        with patch.object(migration, "format_source", side_effect=RuntimeError("formatter unavailable")):
            result = migration.prepare_tree(self.root)
        self.assertTrue(result.unsupported)
        self.assertEqual(dest.read_bytes(), source)
        result = migration.prepare_tree(self.root)
        self.assertFalse(result.unsupported, result.unsupported)
        dest.write_bytes(source + b"\n// concurrent\n")
        with self.assertRaisesRegex(RuntimeError, "changed"):
            migration.install(self.root, result)
        self.assertEqual(dest.read_bytes(), source + b"\n// concurrent\n")
        dest.write_bytes(source)
        second_path = rd.SCOPE + "/z.rs"
        self.put(second_path, f"fn z() {{ let (v, _) = {F}(i); }}")
        result = migration.prepare_tree(self.root)
        originals = {p: (self.root / p).read_bytes() for p in result.changed}
        real_replace = migration.os.replace
        calls = []
        def fail_second(src, dst):
            calls.append(dst)
            if len(calls) == 2:
                raise OSError("installation witness")
            return real_replace(src, dst)
        with patch.object(migration.os, "replace", side_effect=fail_second):
            with self.assertRaisesRegex(RuntimeError, "installation"):
                migration.install(self.root, result)
        self.assertEqual(originals, {p: (self.root / p).read_bytes() for p in originals})

    def test_empty_scope_is_not_readiness(self):
        self.put(P, "fn unrelated() {}")
        result = self.cli("--check")
        self.assertEqual(result.returncode, 2)
        self.assertIn("empty", result.stderr)


class TinyGrammarTests(unittest.TestCase):
    def refuse(self, statement, category="unsupported-scope _rest"):
        before = f"fn f() {{ let (v, _rest) = {F}(i); {statement} consume(v); }}"
        plan = migration.plan_source(P, before)
        self.assertTrue(any(category in error for error in plan.unsupported), plan.unsupported)
        self.assertEqual((plan.rendered, plan.edits), (before, ()))
        control = migration.plan_source(
            P, f"fn f() {{ let (v, _rest) = {F}(i); consume(v, _rest); }}")
        self.assertFalse(control.unsupported, control.unsupported)
        self.assertIn("consume(v, rest)", control.rendered)

    def test_r4_f1_closure_after_block(self):
        for block in ("match v { _ => {} }", "{}", "while c {}", "for x in xs {}"):
            with self.subTest(block=block):
                self.refuse("let n = run({ " + block + " |_rest| _rest.len() });")

    def test_r4_f2_destructuring_assignees(self):
        for statement in ("[_rest] = [other];", "(_rest, k) = (other, k);"):
            with self.subTest(statement=statement):
                self.refuse(statement)

    def test_r4_f3_named_macro_label_and_width(self):
        for literal in ("{:_rest$}", "{:._rest$}", "{:>_rest$}"):
            with self.subTest(literal=literal):
                self.refuse(f'println!("{literal}", v, _rest = 3);')
                self.refuse(f'println!("{literal}", v);',
                            "unsupported implicit format-capture remainder read")

    def test_r4_f4_cfg_block_operands(self):
        for statement in (
                "if { cfg!(debug_assertions) } { check(_rest, i); }",
                "let _ = { cfg!(debug_assertions) } && check(_rest, i);",
                "let _ = !{ cfg!(debug_assertions) } || check(_rest, i);"):
            with self.subTest(statement=statement):
                self.refuse(statement, "unsupported debug-only remainder observation")

    def test_r4_f5_macro_function_discovery(self):
        self.refuse("macro_rules! unused { () => { fn helper() { consume(_rest); } }; }")
        self.refuse("unused! { let unrelated = value; }")

    def test_r4_f6_cooked_escape_capture(self):
        for literal in (r'"\u{7b}_rest}"', r'"\x7b_rest}"'):
            with self.subTest(literal=literal):
                self.refuse(f"println!({literal});",
                            "unsupported implicit format-capture remainder read")
        self.refuse('// unmatched "\n' + r'println!("\u{7b}_rest}");',
                    "unsupported implicit format-capture remainder read")
        for literal in (r'r"\u{7b}_rest}"', r'"{{_rest}}"', r'"\n"'):
            before = f"fn f() {{ let (v, _rest) = {F}(i); println!({literal}); }}"
            plan = migration.plan_source(P, before)
            self.assertFalse(plan.unsupported, plan.unsupported)
            self.assertIn(f".waive({W})", plan.rendered)
        before = (f"fn f() {{ let (v, _rest) = {F}(i); "
                  + r"""let quote = '"'; /* "\u{7b}_rest}" */ consume(v); }""")
        plan = migration.plan_source(P, before)
        self.assertFalse(plan.unsupported, plan.unsupported)

    def test_r4_f7_raw_identifiers(self):
        for statement in ("consume(r#_rest);", "let p = value; p.r#with_rest();",
                          "let p = value; p.r#waive(ResidueWaiver::Unaudited);"):
            with self.subTest(statement=statement):
                self.refuse(statement)

    def test_r4_occurrence_position_battery(self):
        for statement in (
                "_rest = other;", "_rest += other;", "[_rest] = [other];",
                "(_rest, k) = (other, k);", 'println!("{}", _rest = 3);',
                "let g = |_rest| _rest;", "for _rest in values {}",
                "match x { Some(_rest) => (), _ => () }", "if let Some(_rest) = x {}",
                "let S { field: _rest } = x;", "consume(object._rest);",
                "consume(module::_rest);", "{ consume(_rest); }",
                "let g = || consume(_rest);", "println!(_rest);",
                "consume((_rest,));", "consume([_rest]);",
                "consume(S { field: _rest });", "consume(nested(_rest));",
                "consume(_rest.len());", "return consume(_rest);"):
            with self.subTest(statement=statement):
                self.refuse(statement)
        for statement in (
                "#[cfg(test)] consume(_rest);",
                "let _ = { #[cfg(test)] observe(); true } && check(_rest);",
                "if { cfg!(test) } { consume(_rest); }"):
            with self.subTest(statement=statement):
                self.refuse(statement, "unsupported debug-only remainder observation")

    def test_visible_format_literals_only(self):
        binding = f"let (v, _rest) = {F}(i);"
        for body in (
                r'scan_contains(tp.lower, "doesn\u{2019}t untap during"); ' + binding,
                binding + r' scan_contains(tp.lower, "doesn\u{2019}t untap during");',
                r'println!("\u{7b}_rest}"); ' + binding,
                r'if other { println!("\u{7b}_rest}"); } if ready { ' + binding + " }",
                binding + r' let _rest = other(); println!("\u{7b}_rest}");',
                binding + r' assert_eq!("\u{2019}", expected);',
                binding + r' println!("{}", "\u{2019}");'):
            with self.subTest(body=body):
                plan = migration.plan_source(P, "fn f() { " + body + " }")
                self.assertFalse(plan.unsupported, plan.unsupported)
                self.assertIn(f".waive({W})", plan.rendered)
        for statement in (
                r'write!(out, "\u{7b}_rest}");',
                r'assert_eq!(left, right, "\u{7b}_rest}");',
                r'assert!(ready, "\u{7b}_rest}");',
                r'format_args!("\u{7b}_rest}");'):
            with self.subTest(statement=statement):
                self.refuse(statement, "unsupported implicit format-capture remainder read")

    def test_test_module_configuration_is_ambient(self):
        source = (f"#[cfg(test)] mod tests {{ fn f() {{ let (v, _rest) = {F}(i); "
                  f"let (other, _rest) = {F}(j); consume(v, other); }} }}")
        plan = migration.plan_source(P, source)
        self.assertFalse(plan.unsupported, plan.unsupported)
        self.assertEqual(plan.counts["discard-binding"], 2)
        for header in ("#[cfg(test)] mod tests { fn f() {",
                       "#[cfg(test)] mod tests { #[cfg(debug_assertions)] fn f() {"):
            before = header + f"let (v, _rest) = {F}(i); consume(_rest); }} }}"
            result = migration.plan_source(P, before)
            if "debug_assertions" in header:
                self.assertTrue(any("unsupported debug-only remainder observation" in error
                                    for error in result.unsupported), result.unsupported)
            else:
                self.assertFalse(result.unsupported, result.unsupported)
                self.assertIn("consume(rest)", result.rendered)



if __name__ == "__main__":
    unittest.main()
