#!/usr/bin/env python3
"""Gate H decisions: selected helper boundaries, not arbitrary residue dataflow."""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
import subprocess
import sys
from contextlib import redirect_stderr
from io import StringIO
from unittest.mock import patch

_SPEC = importlib.util.spec_from_file_location(
    "residue", Path(__file__).with_name("detect-residue-discards.py"))
rd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(rd)


def hits(src):
    return rd.find_discards("crates/engine/src/parser/probe.rs", src.splitlines())


class DiscardTests(unittest.TestCase):
    def test_s_named_whole_result_bindings(self):
        forms = (
            "let PATTERN = EXPR; consume(NAME);",
            "let PATTERN: (TargetFilter, &str) = EXPR; consume(NAME);",
            "let PATTERN = EXPR else { return; }; consume(NAME);",
            "if let PATTERN = EXPR { consume(NAME); } else {}",
        )
        for modifier in ("", "ref ", "mut ", "ref mut "):
            for template in forms:
                for expr in (
                    "parse_target(i)",
                    "parse_target(\n    i,\n)",
                    "ctx.with_player_antecedents(|s| parse_target(i, s))",
                    "ctx.with_player_scope(scope, |s| { parse_target(i, s) })",
                ):
                    for name, expected in (("_r", ["whole-result"]), ("r", [])):
                        source = template.replace("PATTERN", modifier + name).replace(
                            "EXPR", expr).replace("NAME", name)
                        with self.subTest(modifier=modifier, template=template,
                                          expr=expr, name=name):
                            self.assertEqual([h.family for h in hits(source)], expected)
                    for expr_control in (
                        f"consume({expr})", f"|s| {expr}", f"move |s| {{ {expr} }}",
                    ):
                        source = template.replace("PATTERN", modifier + "_r").replace(
                            "EXPR", expr_control).replace("NAME", "_r")
                        with self.subTest(control=source):
                            self.assertEqual(hits(source), [])

    def test_s_whole_result_match_alternatives(self):
        patterns = (
            ("NAME", 1),
            ("ref NAME", 1),
            ("mut NAME", 1),
            ("ref mut NAME", 1),
            ("NAME | NAME", 2),
            ("| NAME | NAME", 2),
            ("(NAME | NAME)", 2),
            ("(NAME) | (NAME)", 2),
        )
        for pattern, count in patterns:
            for guard in ("", " if keep(&NAME)"):
                for name in ("_", "_unused", "result"):
                    source = ("match parse_target(\n    i,\n) {\n    "
                              + pattern + guard + " => { consume(NAME); },\n}")
                    source = source.replace("NAME", name)
                    with self.subTest(pattern=pattern, guard=guard, name=name):
                        self.assertEqual([h.family for h in hits(source)],
                                         ["whole-result"] * (0 if name == "result" else count))
                    self.assertEqual(hits(source.replace(
                        "parse_target(\n    i,\n)", "consume(parse_target(i))")), [])
        for pattern in ("_unused | (v, _rest)", "(v, _rest) | _unused"):
            with self.subTest(mixed=pattern):
                self.assertCountEqual([h.family for h in hits(
                    f"match parse_target(i) {{ {pattern} if keep() => {{}}, }}")],
                    ["whole-result", "binding"])

    def test_drop_turbofish_preserves_whole_result_classification(self):
        for drop in ("drop", "std::mem::drop", "mem::drop"):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                for comma in ("", ","):
                    call = drop + generic
                    with self.subTest(call=call, comma=comma):
                        self.assertEqual([(h.family, h.symbol) for h in hits(
                            f"fn f(i: &str) {{ {call}(parse_target(i){comma}); }}")],
                            [("whole-result", "parse_target")])
                        for arg in ("consume(parse_target(i))", "unrelated(i)",
                                    "parse_target(i), unrelated(i)"):
                            self.assertEqual(hits(
                                f"fn f(i: &str) {{ {call}({arg}{comma}); }}"), [])
        for source in ("let value = object.drop;", "let value = mem::drop;",
                       "let value = std::mem::drop::<_>;",
                       "object.drop::<_>(parse_target(i));",
                       "(object).drop::<(T, &str)>(parse_target(i));",
                       "object.drop(consume(parse_target(i)));",
                       "object.drop::<_>(consume(parse_target(i)));"):
            with self.subTest(source=source):
                self.assertEqual(hits(source), [])

    def test_drop_methods_do_not_discard_helper_arguments(self):
        for receiver in ("reader", "(reader)", "factory()", "readers[0]"):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                for arg in ("parse_target(i)", "consume(parse_target(i))"):
                    source = f"{receiver}.drop{generic}({arg});"
                    with self.subTest(source=source):
                        self.assertEqual(hits(source), [])
        for prefix in ("..", "a.."):
            for generic in ("", "::<_>", "::<(T, &str)>"):
                source = f"let range = {prefix}drop{generic}(parse_target(i));"
                with self.subTest(source=source):
                    self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                     [("whole-result", "parse_target")])

    def test_bare_block_statements_discard_only_helper_tails(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            for template in (
                "EXPR;",
                "fn f(i: &str) { EXPR; }",
                "fn f(i: &str) { observe(); EXPR; }",
                "fn f(i: &str) { { observe(); } EXPR; }",
            ):
                expr = f"{{ let t = i.trim(); {helper}(t) }}"
                for tail, expected in (
                    (f"{helper}(t)", [("whole-result", helper)]),
                    (f"consume({helper}(t))", []),
                    (f"{helper}(t).1", []),
                    ("unrelated(t)", []),
                ):
                    source = template.replace("EXPR", expr.replace(f"{helper}(t)", tail))
                    with self.subTest(source=source):
                        self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                         expected)

    def test_block_headers_preserve_existing_classification(self):
        for source in (
            "if c { parse_target(i) };",
            "if c {} else { parse_target(i) };",
            "match x { _ => parse_target(i) };",
            "loop { parse_target(i) };",
            "unsafe { parse_target(i) };",
            "fn f() { parse_target(i) };",
            "while ready() { parse_target(i) };",
            "let result = { parse_target(i) };",
        ):
            with self.subTest(source=source):
                self.assertEqual(hits(source), [])

    def test_block_statement_recognizers_do_not_double_count(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            for source, family in (
                (f"let _ = {{ {helper}(i) }};", "whole-result"),
                (f"drop({{ {helper}(i) }});", "whole-result"),
                (f"{{ drop({helper}(i)) }};", "whole-result"),
                (f"{{ let t = i.trim(); {helper}(t) }}.0;", "projection"),
                (f"{{ {helper}(i).0 }};", "projection"),
            ):
                with self.subTest(source=source):
                    self.assertEqual([(h.family, h.symbol) for h in hits(source)],
                                     [(family, helper)])

    def test_q_trailing_comma_drop_arguments(self):
        for drop in ("drop", "std::mem::drop"):
            with self.subTest(drop=drop):
                self.assertEqual([h.family for h in hits(
                    f"{drop}(\n    parse_target(\n        i,\n    ),\n);")],
                    ["whole-result"])
                self.assertEqual(hits(
                    f"{drop}(\n    consume(parse_target(i)),\n);"), [])
                self.assertEqual(hits(
                    f"{drop}(\n    unrelated(i),\n);"), [])
                self.assertEqual(hits(
                    f"{drop}(parse_target(i), unrelated(i));"), [])

    def test_q_trailing_comma_exact_defaults_in_nested_expressions(self):
        for variant in ("Any", "SelfRef"):
            for template in ("EXPR", "consume(Some(\n    EXPR,\n))",
                             "let f = (EXPR, unrelated());"):
                with self.subTest(variant=variant, template=template):
                    expr = f"x\n    .unwrap_or(\n        TargetFilter::{variant},\n    )"
                    self.assertEqual([h.family for h in hits(template.replace("EXPR", expr))],
                                     ["default"])
                    for control in (f"x.unwrap_or(\n    Other::{variant},\n)",
                                    f"x.unwrap_or(\n    make(TargetFilter::{variant}),\n)",
                                    f"x.unwrap_or(TargetFilter::{variant}, unrelated())"):
                        self.assertEqual(hits(template.replace("EXPR", control)), [])

    def test_q_rustfmt_binding_matrix(self):
        forms = (
            "let (filter, REST,) = EXPR; consume(REST);",
            "let (filter, REST,): (TargetFilter, &str) = EXPR; consume(REST);",
            "let (filter, mut REST,) = EXPR; consume(REST);",
            "let (filter, ref REST,) = EXPR; consume(REST);",
            "let (filter, ref mut REST,) = EXPR; consume(REST);",
            "let (filter, REST, ..) = EXPR; consume(REST);",
            "let (Typed(filter), REST,) = EXPR else { return; }; consume(REST);",
            "if let (Typed(filter), REST,) = EXPR { consume(REST); } else {}",
            "match EXPR {\n    (Typed(filter), REST,) => consume(REST),\n}",
        )
        call = "parse_target_with_ctx(\n    text,\n    ctx,\n)"
        exprs = (
            call,
            "ctx.with_player_antecedents(\n    |s| parse_target_with_ctx(i, s),\n)",
            "ctx.with_player_antecedents(\n    |s| {\n"
            "        parse_target_with_ctx(\n            i,\n            s,\n        )\n    },\n)",
            "ctx.with_player_scope(\n    scope,\n"
            "    |s| parse_target_with_ctx(i, s),\n)",
            "ctx.with_player_scope(\n    scope,\n    |s| {\n"
            "        parse_target_with_ctx(\n            i,\n            s,\n        )\n    },\n)",
        )
        for template in forms:
            for expr in exprs:
                for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                    source = template.replace("REST", residue).replace("EXPR", expr)
                    with self.subTest(template=template, expr=expr, residue=residue):
                        self.assertEqual([h.family for h in hits(source)], expected)
                with self.subTest(template=template, unrelated=True, expr=expr):
                    self.assertEqual(hits(template.replace("REST", "_").replace(
                        "EXPR", f"consume(\n    {expr},\n)")), [])

    def test_q_rustfmt_result_and_pattern_wrappers(self):
        for wrapper in ("Some", "Ok"):
            for trailing_pattern_comma in ("", ","):
                for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                    expr = f"{wrapper}(\n    parse_target(\n        i,\n    ),\n)"
                    pattern = f"{wrapper}((filter, {residue},){trailing_pattern_comma})"
                    for template in (
                        "if let PATTERN = EXPR { consume(REST); }",
                        "match EXPR {\n    PATTERN => consume(REST),\n}",
                    ):
                        source = template.replace("PATTERN", pattern).replace(
                            "EXPR", expr).replace("REST", residue)
                        with self.subTest(wrapper=wrapper, comma=trailing_pattern_comma,
                                          residue=residue, template=template):
                            self.assertEqual([h.family for h in hits(source)], expected)

    def test_q_rustfmt_projections_and_whole_results(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i,\n    ctx," if helper == "parse_target_with_syntax" else "i,"
            call = f"{helper}(\n    {args}\n)"
            for expr in (call, f"(\n    {call}\n)"):
                with self.subTest(helper=helper, projection=projection, expr=expr):
                    self.assertEqual([h.family for h in hits(
                        f"let value = {expr}\n    .{projection};")], ["projection"])
                    self.assertEqual(hits(f"let rest = {expr}\n    .1; consume(rest);"), [])
        call = "parse_target(\n    i,\n)"
        for template in ("let _ = EXPR;", "let _: (TargetFilter, &str) = EXPR;", "EXPR;"):
            with self.subTest(template=template):
                self.assertEqual([h.family for h in hits(template.replace("EXPR", call))],
                                 ["whole-result"])
                self.assertEqual(hits(template.replace("EXPR", f"consume(\n    {call},\n)")), [])

    def test_r_known_limit_import_alias_shadowing_is_untracked(self):
        """Aliases never occur in the real parser tree; local shadowing is untracked."""
        imported = "use crate::target::parse_target as parse;\n"
        shadowed = ("let parse = |s| (s.len(), ());\n"
                    "let (n, _) = parse(i);")
        self.assertEqual([(h.family, h.symbol) for h in hits(imported + shadowed)],
                         [("binding", "parse_target")])
        self.assertEqual(hits(shadowed), [])
        self.assertEqual(hits(imported + shadowed.replace(
            "let (n, _) = parse(i);", "let (n, rest) = parse(i); consume(rest);")), [])

    def test_o_refutable_let_else_and_if_let_else(self):
        self.assertEqual([h.family for h in hits(
            "fn probe(input: &str) -> Option<TargetFilter> { "
            "let (TargetFilter::Typed(typed), _) = parse_target(input) "
            "else { return None; }; Some(TargetFilter::Typed(typed)) }")], ["binding"])
        for expr in (
            "parse_target(input)",
            "ctx.with_player_antecedents(|s| parse_target(input))",
            "ctx.with_player_scope(scope, |s| { parse_target(input) })",
        ):
            for annotation in ("", ": (TargetFilter, &str)"):
                for modifier in ("", "mut ", "ref ", "ref mut "):
                    for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                        pattern = f"(TargetFilter::Typed(typed), {modifier}{residue})"
                        src = (f"fn probe(input: &str) -> Option<TargetFilter> {{ "
                               f"let {pattern}{annotation} = {expr} else {{ return None; }}; "
                               f"consume({residue}); Some(TargetFilter::Typed(typed)) }}")
                        with self.subTest(expr=expr, annotation=annotation,
                                          modifier=modifier, residue=residue):
                            self.assertEqual([h.family for h in hits(src)], expected)
            for residue, expected in (("_rest", ["binding"]), ("rest", [])):
                src = (f"if let (TargetFilter::Typed(typed), {residue}) = {expr} "
                       "{ consume(" + residue + "); } else { return None; }")
                with self.subTest(expr=expr, residue=residue):
                    self.assertEqual([h.family for h in hits(src)], expected)

    def test_p_or_patterns_count_each_discarding_alternative(self):
        for pattern in (
            "(TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST)",
            "| (TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST)",
            "((TargetFilter::Any, REST) | (TargetFilter::SelfRef, REST))",
            "((TargetFilter::Any, REST)) | ((TargetFilter::SelfRef, REST))",
        ):
            for residue, expected in (("_", 2), ("rest", 0)):
                body = "None" if residue == "_" else "consume(rest)"
                src = ("match parse_target(input) { " + pattern.replace("REST", residue)
                       + " => " + body + ", "
                       "(filter, rest) => rest.is_empty().then_some(filter), }")
                with self.subTest(pattern=pattern, residue=residue):
                    self.assertEqual([h.family for h in hits(src)], ["binding"] * expected)
        for pattern in (
            "(TargetFilter::Any, _) | (TargetFilter::SelfRef, rest)",
            "(TargetFilter::Any, rest) | (TargetFilter::SelfRef, _)",
        ):
            with self.subTest(mixed=pattern):
                self.assertEqual([h.family for h in hits(
                    f"match parse_target(input) {{ {pattern} => consume(rest), }}")],
                    ["binding"])
        combined = ("match parse_target(input) { "
                    "(TargetFilter::Any, _) | (TargetFilter::SelfRef, _) => None, }")
        split = combined.replace(" | ", " => None, ")
        self.assertEqual(len(hits(combined)), 2)
        self.assertEqual(len(hits(split)), 2)

    def test_supported_binding_matrix_discarded_vs_consumed(self):
        # Deterministic cross-product, not randomized fuzzing: each grammar axis
        # carries a consumed-remainder control, including inside both wrappers.
        forms = (
            ("plain", "let (filter, REST) = EXPR; consume(REST);", 1),
            ("typed", "let (filter, REST): (TargetFilter, &str) = EXPR; consume(REST);", 1),
            ("mut", "let (filter, mut REST) = EXPR; consume(REST);", 1),
            ("ref", "let (filter, ref REST) = EXPR; consume(REST);", 1),
            ("ref-mut", "let (filter, ref mut REST) = EXPR; consume(REST);", 1),
            ("tuple-rest", "let (filter, REST, ..) = EXPR; consume(REST);", 1),
            ("let-else", "let (Typed(filter), REST) = EXPR else { return; }; consume(REST);", 1),
            ("if-let", "if let (Typed(filter), REST) = EXPR { consume(REST); } else {}", 1),
            ("match", "match EXPR { (Typed(filter), REST) => consume(REST), }", 1),
            ("or-pattern", "match EXPR { (Any, REST) | (SelfRef, REST) => consume(REST), }", 2),
        )
        for helper in ("parse_target", "parse_target_with_syntax"):
            call = f"{helper}(input)"
            multiline = f"{helper}(\n    input,\n)"
            for expr in (call, f"ctx.with_player_antecedents(|s| {call})",
                         f"ctx.with_player_scope(scope, |s| {{ {call} }})",
                         multiline,
                         f"ctx.with_player_antecedents(\n    |s| {multiline},\n)",
                         f"ctx.with_player_scope(\n    scope,\n    |s| {{ {multiline} }},\n)"):
                for name, template, discards in forms:
                    for residue in ("_rest", "rest"):
                        src = template.replace("REST", residue).replace("EXPR", expr)
                        if helper == "parse_target_with_syntax":
                            src = src.replace(", " + residue + ")", ", " + residue + ", syntax)")
                            src = src.replace("(TargetFilter, &str)", "(TargetFilter, &str, Syntax)")
                        elif name == "tuple-rest":
                            pattern = "(filter, ..)" if residue == "_rest" else "(.., rest)"
                            src = src.replace(f"(filter, {residue}, ..)", pattern)
                        with self.subTest(helper=helper, expr=expr, form=name, residue=residue):
                            self.assertEqual([h.family for h in hits(src)],
                                             ["binding"] * (discards if residue == "_rest" else 0))

    def test_h_block_closure_tail_after_expression_statements(self):
        for statement in ("if input.is_empty() { observe(); }",
                          "if flag { observe(); } else { observe(); }",
                          "match flag { true => {}, false => {} }",
                          "unsafe { observe(); }", "loop { break; }",
                          "while flag { break; }", "for x in xs { observe(); }",
                          "if let Foo { a } = prior { observe(a); }",
                          "if let Foo { a } = prior { observe(a); } else { observe(); }",
                          "if let Foo { a } = prior && let Bar { b } = a { observe(b); }",
                          "while let Foo { a } = it.next() { observe(a); }",
                          "for Foo { a } in xs { observe(a); }",
                          "for (Foo { a }, 0..=3) in xs { observe(a); }",
                          "match prior { Foo { a } => observe(a), _ => {} }"):
            expr = ("ctx.with_player_antecedents(|s| { " + statement
                    + " parse_target_with_ctx(input, s) })")
            with self.subTest(statement=statement):
                self.assertEqual([h.family for h in hits(f"let (v, _) = {expr};")], ["binding"])
                self.assertEqual(hits(f"let (v, rest) = {expr}; consume(rest);"), [])

    def test_i_comma_free_expression_with_block_match_arms(self):
        for body in ("if rest.is_empty() { observe(); }",
                     "if flag { consume(rest); } else { consume(rest); }",
                     "match flag { true => consume(rest), false => () }",
                     "unsafe { consume(rest); }", "loop { consume(rest); break; }",
                     "while flag { consume(rest); break; }",
                     "for x in xs { consume(rest); }",
                     "if let Foo { a } = prior { consume(rest); }",
                     "for Foo { a } in xs { consume(rest); }",
                     "while let Foo { a } = it.next() { consume(rest); break; }"):
            with self.subTest(body=body):
                prefix = "match parse_target(input) { (TargetFilter::Any, rest) => " + body
                self.assertEqual([h.family for h in hits(prefix + " (v, _) => use_it(v), }")],
                                 ["binding"])
                self.assertEqual(hits(prefix + " (v, rest) => consume(rest), }"), [])

    def test_l_turbofish_commas_do_not_end_a_match_arm_expression(self):
        # Generic-argument commas are not arm separators: the discard arm that follows
        # a turbofish-bearing arm expression must still be counted, and a consumed
        # remainder must still count nothing.
        for call in ("tag::<_, _, E>(\"tail\")",
                     "tag::<_, _, OracleError<'_>>(\"tail\")",
                     "collect::<Vec<(A, B)>>()",
                     "nom::combinator::opt::<_, _, (E, F)>(inner)",
                     "tag::<_, _, E>(\"a\").or(tag::<_, _, E>(\"b\"))"):
            with self.subTest(call=call):
                prefix = ("match parse_target(input) { (TargetFilter::Any, rest) => "
                          f"{call}.parse(rest).is_ok(), ")
                self.assertEqual([h.family for h in hits(prefix + "(v, _) => use_it(v), }")],
                                 ["binding"])
                self.assertEqual(hits(prefix + "(v, rest) => consume(rest), }"), [])
        # A comparison is not a turbofish: its `<` and `>` must not swallow the arm.
        self.assertEqual([h.family for h in hits(
            "match parse_target(input) { (TargetFilter::Any, rest) => a < b, "
            "(v, _) => use_it(v), }")], ["binding"])

    def test_block_initializer_postfix_is_not_a_bare_helper_result(self):
        # Only registered result-preserving wrappers are followed. Projections
        # and arbitrary methods (even clone) must agree with the non-block form.
        for postfix in (".1.is_empty()", ".1.clone()", ".clone()",
                        ".map(|x| { x })"):
            for pattern in ("_rest_is_empty", "(filter, _)"):
                for expr in ("parse_target(input)",
                             "{ let input = input.trim(); parse_target(input) }"):
                    with self.subTest(postfix=postfix, pattern=pattern, expr=expr):
                        self.assertEqual(hits(f"let {pattern} = {expr}{postfix};"), [])

    def test_method_projections_do_not_resolve_helper_aliases_or_names(self):
        for receiver in ("reader", "(reader)", "factory()", "readers[0]",
                         "a.b().c", "self.x"):
            for callee, imports in (
                ("read", "use crate::target::parse_target as read; "),
                ("parse_target", ""),
            ):
                for projection in ("0", "2", "1"):
                    with self.subTest(receiver=receiver, callee=callee,
                                      projection=projection):
                        self.assertEqual(hits(
                            imports + f"let rest = {receiver}.{callee}(i).{projection}; "
                            "consume(rest);"), [])

    def test_range_endpoint_projections_keep_discard_obligations(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            for receiver in (call, f"crate::target::{call}", f"({call})",
                             f"{{ {call} }}"):
                for prefix in ("..", "a..", "..="):
                    with self.subTest(helper=helper, projection=projection,
                                      receiver=receiver, prefix=prefix):
                        self.assertEqual([(h.family, h.symbol) for h in hits(
                            f"let value = {prefix}{receiver}.{projection};")],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            f"let rest = {prefix}{receiver}.1; consume(rest);"), [])

    def test_free_helper_projections_keep_discard_obligations(self):
        for call, helper, projection, imports in (
            ("read(i)", "parse_target", "0",
             "use crate::target::parse_target as read; "),
            ("parse_target(i)", "parse_target", "0", ""),
            ("crate::target::parse_target(i)", "parse_target", "0", ""),
            ("parse_target_with_syntax(i, c)", "parse_target_with_syntax", "2", ""),
        ):
            block = "{ let i = i.trim(); " + call + " }"
            for expr in (call, f"({call})", block, f"({block})"):
                for template in ("let value = EXPR; consume(value);",
                                 "fn f() -> T { return EXPR; }",
                                 "fn f() -> T { loop { break EXPR; } }"):
                    with self.subTest(call=call, expr=expr, template=template):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                imports + template.replace("EXPR", f"{expr}.{projection}"))],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            imports + template.replace("EXPR", f"{expr}.1")), [])

    def test_registered_wrapper_projections_keep_discard_obligations(self):
        for wrapper in ("ctx.with_player_antecedents(|s| CALL)",
                        "ctx.with_player_scope(scope, |s| { CALL })"):
            for call, helper, projection in (
                ("parse_target(i)", "parse_target", "0"),
                ("parse_target_with_syntax(i, c)", "parse_target_with_syntax", "2"),
            ):
                wrapped = wrapper.replace("CALL", call)
                for expr in (wrapped, f"({wrapped})"):
                    with self.subTest(wrapper=wrapper, call=call, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                f"let value = {expr}.{projection}; consume(value);")],
                            [("projection", helper)])
                        self.assertEqual(hits(
                            f"let rest = {expr}.1; consume(rest);"), [])

    def test_bare_block_projections_match_parenthesized_and_call_forms(self):
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            block = "{ let i = input.trim(); " + call + " }"
            for expr in (block, f"({block})", call):
                for name in ("_filter", "filter"):
                    with self.subTest(helper=helper, projection=projection,
                                      expr=expr, name=name):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                f"let {name} = {expr}.{projection};")],
                            [("projection", helper)])

    def test_block_projections_in_return_and_break_operands(self):
        contexts = (
            "fn f(i: &str, e: bool) -> T { if e { return EXPR; } T::Any }",
            "fn f(i: &str) -> T { loop { break EXPR; } }",
            "fn f(i: &str) -> T { 'a: loop { break 'a EXPR; } }",
        )
        for helper, projection in (("parse_target", "0"),
                                   ("parse_target_with_syntax", "2")):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            call = f"{helper}({args})"
            block = "{ let i = i.trim(); " + call + " }"
            for context in contexts:
                for expr in (block, f"({block})", call):
                    with self.subTest(helper=helper, context=context, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(
                                context.replace("EXPR", f"{expr}.{projection}"))],
                            [("projection", helper)])

    def test_return_and_break_operands_preserve_remainder_controls(self):
        for prefix in ("return", "break", "break 'a"):
            for helper in ("parse_target", "parse_target_with_syntax"):
                args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
                call = f"{helper}({args})"
                block = "{ let i = i.trim(); " + call + " }"
                for expr in (block, f"({block})", call):
                    for postfix in (".1", ".1.len()"):
                        with self.subTest(prefix=prefix, helper=helper,
                                          expr=expr, postfix=postfix):
                            self.assertEqual(hits(
                                "fn f(i: &str) { 'a: loop { "
                                f"{prefix} {expr}{postfix};"
                                " } }"), [])
                with self.subTest(prefix=prefix, helper=helper):
                    self.assertEqual(hits(
                        "fn f(i: &str) { 'a: loop { "
                        f"let (filter, rest) = {block}; consume(rest); "
                        f"{prefix} filter;"
                        " } }"), [])

    def test_bare_block_non_discard_postfixes_and_consumed_remainder(self):
        for helper in ("parse_target", "parse_target_with_syntax"):
            args = "i, ctx" if helper == "parse_target_with_syntax" else "i"
            block = "{ let i = input.trim(); " + f"{helper}({args})" + " }"
            for postfix in (".1", ".1.is_empty()", ".clone()"):
                for name in ("_rest", "rest"):
                    with self.subTest(helper=helper, postfix=postfix, name=name):
                        self.assertEqual(hits(f"let {name} = {block}{postfix};"), [])
        block = "{ let i = input.trim(); parse_target(i) }"
        self.assertEqual(hits(f"let _syntax = {block}.2;"), [])
        self.assertEqual(hits(f"let (f, rest) = {block}; consume(rest);"), [])

    def test_projection_does_not_treat_keyword_bodies_as_standalone_blocks(self):
        # These expression bodies were not helper-result receivers before bare
        # block projections were supported; the closing brace alone is not enough.
        for call, projection in (("parse_target(i)", "0"),
                                 ("parse_target_with_syntax(i, ctx)", "2")):
            for expr in ("if c { CALL } else { CALL }",
                         "match x { _ => CALL }",
                         "unsafe { CALL }",
                         "loop { CALL }",
                         "|i| -> (TargetFilter, &str) { CALL }",
                         "Foo { value: CALL }"):
                for name in ("_filter", "filter"):
                    with self.subTest(call=call, expr=expr, name=name):
                        self.assertEqual(hits(
                            f"let {name} = {expr.replace('CALL', call)}.{projection};"), [])

    def test_block_initializer_bare_and_question_mark_keep_discard_obligations(self):
        for expr in ("{ let input = input.trim(); parse_target(input) }",
                     "{ let input = input.trim(); parse_target(input) }?"):
            for pattern, expected in (("(filter, _)", ["binding"]),
                                      ("_result", ["whole-result"]),
                                      ("(filter, rest)", [])):
                with self.subTest(expr=expr, pattern=pattern):
                    self.assertEqual(
                        [h.family for h in hits(f"let {pattern} = {expr};")], expected)

    def test_block_initializer_postfix_stops_before_if_let_body_and_let_else(self):
        for template in (
            "if let PATTERN = EXPR { observe(); } else { observe(); }",
            "let PATTERN = EXPR else { return; }; observe();",
        ):
            for expr, expected in (
                ("{ let input = input.trim(); parse_target(input) }", ["binding"]),
                ("{ let input = input.trim(); parse_target(input) }?", ["binding"]),
                ("{ let input = input.trim(); parse_target(input) }.1.clone()", []),
                ("{ let input = input.trim(); parse_target(input) }.map(|x| { x })", []),
            ):
                source = template.replace("PATTERN", "(Typed(filter), _)").replace("EXPR", expr)
                with self.subTest(template=template, expr=expr):
                    self.assertEqual([h.family for h in hits(source)], expected)

    def test_j_aliases_are_scoped_to_lexical_modules(self):
        src = ("mod a { use crate::target::parse_target_with_syntax as read; "
               "fn f(i: &str) { let s = read(i, ctx).2; } }\n"
               "mod b { use crate::target::parse_target as read; "
               "fn g(i: &str) { let (v, rest) = read(i); consume(rest); } }\n"
               "mod c { fn read(i: &str) -> (u8, u8) { (0, 0) } "
               "fn h(i: &str) { let (v, _) = read(i); } }")
        self.assertEqual([(h.line, h.family, h.symbol) for h in hits(src)],
                         [(1, "projection", "parse_target_with_syntax")])
        self.assertEqual(hits(src.replace("read(i, ctx).2", "read(i, ctx).1")), [])

    def test_j_aliases_only_resolve_unqualified_calls(self):
        cases = (
            ("use crate::target::parse_target as read; "
             "fn f(input: &str) { let (rest, _) = other::read(input); consume(rest); }",
             []),
            ("use other::read as parse_target; "
             "fn f(input: &str) { let (filter, _) = crate::target::parse_target(input); }",
             [("binding", "parse_target")]),
            ("use crate::target::parse_target as read; "
             "fn f(input: &str) { let (filter, _) = read(input); }",
             [("binding", "parse_target")]),
        )
        for source, expected in cases:
            with self.subTest(source=source):
                self.assertEqual([(h.family, h.symbol) for h in hits(source)], expected)
                consumed = source.replace("(filter, _) =", "(filter, rest) =").replace(
                    "input); }", "input); consume(rest); }")
                self.assertEqual(hits(consumed), [])

    def test_j_qualified_reexport_does_not_use_callers_alias(self):
        src = ("mod a { pub use crate::target::parse_target as read; }\n"
               "mod b { use crate::target::parse_target_with_syntax as read; "
               "fn f(input: &str) { let (filter, _) = crate::a::read(input); } }")
        self.assertEqual(hits(src), [])
        self.assertEqual(hits(src.replace(
            "(filter, _) = crate::a::read(input);",
            "(filter, rest) = crate::a::read(input); consume(rest);")), [])

    def test_j_qualified_helpers_ignore_alias_shadowing(self):
        for helper in sorted(rd.HELPERS):
            syntax = ", syntax" if helper == "parse_target_with_syntax" else ""
            for qualifier in ("self::", "crate::", "super::", "a::b::", "::"):
                with self.subTest(helper=helper, qualifier=qualifier):
                    prefix = f"use other::read as {helper}; fn f(input: &str) {{ "
                    call = f"{qualifier}{helper}(input, ctx);"
                    self.assertEqual(
                        [(h.family, h.symbol) for h in hits(
                            prefix + f"let (filter, _{syntax}) = " + call + " }")],
                        [("binding", helper)])
                    self.assertEqual(hits(
                        prefix + f"let (filter, rest{syntax}) = " + call
                        + " consume(rest); }"), [])

    def test_k_binding_modifiers_preserve_ignored_remainder(self):
        for modifier in ("mut", "ref", "ref mut"):
            with self.subTest(modifier=modifier):
                self.assertEqual([h.family for h in hits(
                    f"let (value, {modifier} _rest) = parse_target(i);")], ["binding"])
                self.assertEqual(hits(
                    f"let (value, {modifier} rest) = parse_target(i); consume(rest);"), [])

    def test_l_typed_wildcard_drops_whole_result(self):
        self.assertEqual([h.family for h in hits(
            "let _: (TargetFilter, &str) = parse_target(input);")], ["whole-result"])
        self.assertEqual(hits(
            "let result: (TargetFilter, &str) = parse_target(input); consume(result.1);"), [])

    def test_m_uncalled_closure_is_not_a_helper_result(self):
        for closure in ("|| parse_target(input)", "move || parse_target(input)",
                        "|s| { parse_target_with_ctx(input, s) }"):
            with self.subTest(closure=closure):
                src = f"let _ = {closure};\nlet (v, _) = parse_target(witness);"
                self.assertEqual([(h.line, h.family) for h in hits(src)], [(2, "binding")])

    def test_n_precise_capture_is_not_an_import(self):
        capture = "fn keep<'a>(i: &'a str) -> impl Fn() -> &'a str + use<'a> { move || i }"
        self.assertEqual(hits(capture), [])
        self.assertEqual([h.family for h in hits(
            capture + "\nfn f() { let (v, _) = parse_target(input); }")], ["binding"])

    def test_n_precise_capture_module_discovery(self):
        capture = "fn keep<'a>(i: &'a str) -> impl Fn() -> &'a str + use<'a> { move || i }"
        for source in (capture + "\nmod child;", "mod child;\n" + capture):
            with self.subTest(source=source):
                self.assertEqual(self.discovery_hits({
                    "mod.rs": source,
                    "child.rs": "fn f() { let (v, _) = parse_target(input); }",
                }), [("child.rs", "binding")])

    def test_n_import_item_visibilities(self):
        for visibility in ("", "pub ", "pub(crate) "):
            self.assertEqual([h.symbol for h in hits(
                f"{visibility}use crate::target::parse_target as read;\n"
                "fn f() { let (v, _) = read(input); }")], ["parse_target"])

    def test_n_internal_errors_name_file_without_traceback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parser = root / rd.SCOPE
            parser.mkdir(parents=True)
            (parser / "probe.rs").write_text("fn f() {}")
            for operation in ("module_edges", "find_discards"):
                stderr = StringIO()
                with self.subTest(operation=operation):
                    with patch.object(rd, operation, side_effect=RuntimeError("injected failure")), \
                            patch("sys.argv", ["detector", "--list", "--root", tmp]), \
                            redirect_stderr(stderr):
                        self.assertEqual(rd.main(), 1)
                    self.assertIn("detector internal error at crates/engine/src/parser/probe.rs",
                                  stderr.getvalue())
                    self.assertIn("RuntimeError: injected failure", stderr.getvalue())
                    self.assertNotIn("Traceback", stderr.getvalue())

    def test_helper_table_and_discard_bindings(self):
        for helper in rd.HELPERS:
            for rest in ("_", "_rem", "_rest"):
                syntax = ", syntax" if helper == "parse_target_with_syntax" else ""
                with self.subTest(helper=helper, rest=rest):
                    found = hits(f"let (value, {rest}{syntax}) = crate::target::{helper}(input, ctx);")
                    self.assertEqual([(h.family, h.symbol) for h in found], [("binding", helper)])

    def test_unknown_closure_consumers_do_not_forward_helper_results(self):
        for consumer in ("into_nom(|| parse_target(input))",
                         "ctx.into_nom(|s| parse_target(input))"):
            src = ("fn into_nom(f: impl FnOnce() -> (TargetFilter, &str)) -> (&str, ()) "
                   "{ let (_, rest) = f(); (rest, ()) }\n"
                   f"let (rest, _) = {consumer};\n"
                   "let (value, _) = parse_target(witness);")
            self.assertEqual([(h.line, h.family) for h in hits(src)], [(3, "binding")])

    def test_established_scope_wrapper_forwards_last_closure_result(self):
        src = "let (value, _) = ctx.with_player_scope(scope, |s| parse_target_with_ctx(input, s));"
        self.assertEqual([(h.family, h.symbol) for h in hits(src)],
                         [("binding", "parse_target_with_ctx")])

    def test_scope_wrapper_closure_return_annotations(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for annotation in ("", " -> (TargetFilter, &'a str)",
                               " -> (TargetFilter, &str)",
                               " -> Result<(TargetFilter, &'a str), E>",
                               " -> Option<Vec<(TargetFilter, &'a str)>>",
                               " -> Result<fn() -> (TargetFilter, &'a str), E>"):
                for capture in ("", "move "):
                    for statement in ("", "let input = prepare(input); observe(); "):
                        expr = (f"ctx.{wrapper}({prefix}{capture}|s|{annotation} {{ "
                                f"{statement}parse_target_with_ctx(input, s) }},)")
                        with self.subTest(wrapper=wrapper, annotation=annotation,
                                          capture=capture, statement=statement):
                            self.assertEqual(
                                [(h.family, h.symbol) for h in hits(f"let (filter, _) = {expr};")],
                                [("binding", "parse_target_with_ctx")])
                            self.assertEqual(
                                hits(f"let (filter, rest) = {expr}; consume(rest);"), [])
            expr = f"ctx.{wrapper}({prefix}|s| -> u8 {{ 0 }})"
            with self.subTest(wrapper=wrapper, result="not a helper"):
                self.assertEqual(hits(f"let (filter, _) = {expr};"), [])

    def test_scope_wrapper_compound_receivers(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "self.ctx().inner[0]",
                             "get_context()?"):
                for closure in ("|s| parse_target_with_ctx(input, s)",
                                "move |s| -> Result<(TargetFilter, &'a str), E> "
                                "{ parse_target_with_ctx(input, s) },"):
                    expr = f"{receiver}.{wrapper}({prefix}{closure})"
                    with self.subTest(wrapper=wrapper, receiver=receiver, closure=closure,
                                      result="discarded remainder"):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(f"let (filter, _) = {expr};")],
                            [("binding", "parse_target_with_ctx")])
                    with self.subTest(wrapper=wrapper, receiver=receiver, closure=closure,
                                      result="consumed remainder"):
                        self.assertEqual(
                            hits(f"let (filter, rest) = {expr}; consume(rest);"), [])

    def test_scope_wrapper_compound_receiver_projections(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "get_context()?",
                             "self.ctx().inner[0]"):
                for helper in ("parse_target_with_ctx", "parse_target_with_syntax"):
                    wrapped = f"{receiver}.{wrapper}({prefix}|s| {helper}(input, s))"
                    for expr in (wrapped, f"({wrapped})"):
                        for projection in (("0", "2") if helper == "parse_target_with_syntax"
                                           else ("0",)):
                            with self.subTest(wrapper=wrapper, receiver=receiver,
                                              helper=helper, expr=expr, projection=projection):
                                self.assertEqual(
                                    [(h.family, h.symbol) for h in hits(
                                        f"let f = {expr}.{projection};")],
                                    [("projection", helper)])
                        with self.subTest(wrapper=wrapper, receiver=receiver,
                                          helper=helper, expr=expr, projection="1"):
                            self.assertEqual(hits(f"let rest = {expr}.1; consume(rest);"), [])
                expr = f"{receiver}.with_other(|s| parse_target_with_ctx(input, s)).0"
                with self.subTest(control=expr):
                    self.assertEqual(hits(f"let f = {expr};"), [])
                # Tuple projection is postfix: it binds before the binary operator.
                for operator in ("+", "-", "*", "/", "%", "&", "|", "^",
                                 "<", ">", "==", "!=", "&&", "||"):
                    expr = (f"a {operator} {receiver}.{wrapper}({prefix}|s| "
                            "parse_target_with_ctx(input, s)).0")
                    with self.subTest(operator=operator, expr=expr):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(f"let f = {expr};")],
                            [("projection", "parse_target_with_ctx")])

    def test_scope_wrapper_projection_keyword_boundaries(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("contexts[0]", "get_context()", "get_context()?",
                             "self.ctx().inner[0]", "(ctx)"):
                expr = (f"{receiver}.{wrapper}({prefix}|s| "
                        "parse_target_with_ctx(input, s)).0")
                for template in ("if EXPR {}", "match EXPR { _ => () }",
                                 "return EXPR;", "for f in EXPR {}"):
                    with self.subTest(wrapper=wrapper, receiver=receiver, template=template):
                        self.assertEqual(
                            [(h.family, h.symbol) for h in hits(template.replace("EXPR", expr))],
                            [("projection", "parse_target_with_ctx")])

    def test_scope_wrapper_compound_receiver_guards(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("a + ctx", "x == ctx"):
                expr = f"{receiver}.{wrapper}({prefix}|s| parse_target_with_ctx(input, s))"
                with self.subTest(wrapper=wrapper, receiver=receiver):
                    self.assertEqual(hits(f"let (filter, _) = {expr};"), [])
            for receiver in ("contexts[0]", "get_context()", "self.ctx().inner[0]",
                             "get_context()?"):
                expr = f"{receiver}.{wrapper}({prefix}|s| -> u8 {{ 0 }})"
                with self.subTest(wrapper=wrapper, receiver=receiver, result="not a helper"):
                    self.assertEqual(hits(f"let (filter, _) = {expr};"), [])

    def test_scope_wrapper_turbofish_results(self):
        for wrapper, prefix in (("with_player_antecedents", ""),
                                ("with_player_scope", "scope, ")):
            for receiver in ("ctx", "contexts[0]", "get_context::<T>()",
                             "Self::get_context::<Vec<(A, B)>>()",
                             "get_context::<fn() -> T>()"):
                for generic in ("", "::<_>", "::<Vec<(A, B)>>", "::<fn() -> T>"):
                    expr = (f"{receiver}.{wrapper}{generic}({prefix}|s| "
                            "parse_target_with_ctx(input, s))")
                    for template, expected in (
                        ("let (f, _) = EXPR;", [("binding", "parse_target_with_ctx")]),
                        ("let (f, rest) = EXPR; consume(rest);", []),
                        ("let f = EXPR.0;", [("projection", "parse_target_with_ctx")]),
                        ("let rest = EXPR.1; consume(rest);", []),
                    ):
                        with self.subTest(wrapper=wrapper, receiver=receiver,
                                          generic=generic, template=template):
                            self.assertEqual(
                                [(h.family, h.symbol) for h in hits(
                                    template.replace("EXPR", expr))], expected)
                    unregistered = expr.replace(wrapper, "with_other")
                    for template in ("let (f, _) = EXPR;", "let f = EXPR.0;"):
                        with self.subTest(unregistered=unregistered, template=template):
                            self.assertEqual(hits(template.replace("EXPR", unregistered)), [])

    def test_typed_binding_discards_residue(self):
        found = hits("let (filter, _): (TargetFilter, &str) = parse_target(input);")
        self.assertEqual([(h.family, h.symbol) for h in found], [("binding", "parse_target")])
        self.assertEqual(hits("let (filter, rest): (TargetFilter, &str) = parse_target(input); consume(rest);"), [])

    def test_tuple_rest_patterns_discard_only_when_covering_residue(self):
        for pattern, helper in (("(filter, ..)", "parse_target"),
                                ("(..)", "parse_target"),
                                ("(filter, ..)", "parse_target_with_syntax"),
                                ("(.., syntax)", "parse_target_with_syntax")):
            with self.subTest(pattern=pattern, helper=helper):
                self.assertEqual([h.family for h in hits(f"let {pattern} = {helper}(input);")], ["binding"])
        for pattern, helper in (("(.., rest)", "parse_target"),
                                ("(filter, rest, ..)", "parse_target_with_syntax")):
            with self.subTest(pattern=pattern, helper=helper):
                self.assertEqual(hits(f"let {pattern} = {helper}(input); consume(rest);"), [])

    def test_multiline_unbounded_and_wrapped_closures(self):
        for expr in (
            "parse_target_with_ctx(\n" + "input,\n" * 400 + "ctx)",
            "ctx.with_player_antecedents(|s| parse_target_with_ctx(input, s))",
            "ctx.with_player_antecedents(|s| { let x = input; parse_target_with_ctx(x, s) })",
            "ctx.with_player_scope(scope, move |s| { parse_target_with_ctx(input, s) })",
            "(parse_target_with_ctx(input, ctx))",
        ):
            self.assertEqual(len(hits(f"let (value, _) = {expr};")), 1)

    def test_if_let_and_match(self):
        self.assertEqual(len(hits("if let Some((value, _)) = Some(parse_target(i)) { use_it(value); }")), 1)
        self.assertEqual(len(hits("match parse_target(i) { (value, _) => use_it(value), }")), 1)
        self.assertEqual(len(hits("match Some(parse_target(i)) { Some((value, _rest)) => use_it(value), None => (), }")), 1)

    def test_projections(self):
        for expr in ("parse_target(i).0", "(parse_target(i)).0", "parse_target_with_syntax(i, ctx).2"):
            self.assertEqual([h.family for h in hits(f"let value = {expr};")], ["projection"])
        self.assertEqual(hits("let rest = parse_target(i).1;"), [])

    def test_whole_result_drops(self):
        for expr in ("let _ = parse_target(i);", "drop(parse_target(i));", "parse_target(i);",
                     "std::mem::drop((parse_target(i)));", "let _ = (parse_target(i));",
                     "(parse_target(i));", "ctx.with_player_antecedents(|s| parse_target(i, s));"):
            self.assertEqual([h.family for h in hits(expr)], ["whole-result"])

    def test_alias_imports(self):
        for imp in ("use crate::target::parse_target as read;", "use crate::target::{parse_target as read, Other};"):
            self.assertEqual([h.symbol for h in hits(imp + "\nlet (v, _) = read(i);")], ["parse_target"])

    def test_exact_defaults_independent_of_producer(self):
        for variant in ("SelfRef", "Any"):
            self.assertEqual([h.family for h in hits(f"let f = producer().unwrap_or(TargetFilter::{variant});")], ["default"])
        self.assertEqual(hits("let f = producer().unwrap_or_else(|| TargetFilter::Any);"), [])
        self.assertEqual(hits("let f = producer().unwrap_or(Other::Any);"), [])

    def test_nine_default_risk_sites_are_counted_without_producer_assumptions(self):
        producers = ("hand().map(|(_, t)| t)",) * 3 + (
            "scan()", "prevention().filter(|t| valid(t))", "def.affected.take()",
            "subject()", "attached().map(|(f, _)| f)", "filter.clone()")
        variants = ("Any", "Any", "Any", "SelfRef", "Any", "Any", "SelfRef", "SelfRef", "Any")
        found = hits("\n".join(f"let f = {p}.unwrap_or(TargetFilter::{v});"
                               for p, v in zip(producers, variants)))
        self.assertEqual([h.family for h in found], ["default"] * 9)

    def test_consumed_results_are_not_ignored_statements(self):
        for src in ("fn f() { return parse_target(i); }",
                    "fn f() { parse_target(i) }",
                    "let result = parse_target(i);",
                    "let (v, rest) = ctx.with_player_antecedents(|s| { parse_target(i) });"):
            self.assertEqual(hits(src), [])
        self.assertEqual([h.family for h in hits("fn f() { return parse_target(i).0; }")],
                         ["projection"])

    def test_guarded_match_arm_discards_are_counted(self):
        src = ("match parse_target(i) { (v, _) if keep(&v) => use_it(v), "
               "(v, rest) => consume(rest), }")
        self.assertEqual([h.family for h in hits(src)], ["binding"])

    def test_block_match_arms_need_no_trailing_comma(self):
        for src, expected in (
            ("match parse_target(i) { (Typed(v), rest) => { consume(rest); } "
             "(v, _) => { use_it(v); } }", 1),
            ("match parse_target(i) { (Typed(v), _) => { use_it(v); } "
             "(v, _) if keep(&v) => { use_it(v); } }", 2),
        ):
            with self.subTest(src=src):
                self.assertEqual([h.family for h in hits(src)], ["binding"] * expected)

    def test_block_arm_followed_by_or_pattern_starting_with_a_tuple(self):
        for src, expected in (
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, _) | (SelfRef, _) => {} (_, rest) => { consume(rest); } }", 2),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, _) | (SelfRef, _) if keep() => {} (_, rest) => { consume(rest); } }", 2),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "[Any, _] | [SelfRef, _] => {} }", 0),
            ("match parse_target(i) { (Typed(_), rest) => { consume(rest); } "
             "(Any, r) | (SelfRef, r) => { consume(r); } }", 0),
        ):
            with self.subTest(src=src):
                self.assertEqual([h.family for h in hits(src)], ["binding"] * expected)

    def test_each_match_arm_discard_is_a_distinct_syntactic_occurrence(self):
        self.assertEqual(len(hits("match parse_target(i) { (Typed(t), _) => t, (other, _) => other, }")), 2)

    def test_nom_unit_outputs_with_production_witness(self):
        for helper in ("parse_target_demonstrative_subject", "parse_target_anaphoric_subject"):
            src = (f"fn {helper}(i: &str) -> OracleResult<'_, ()> {{ Ok((i, ())) }}\n"
                   f"let (rest, _) = {helper}(input)?;\n"
                   "let (value, _) = parse_target(input);")
            self.assertEqual([(h.line, h.family) for h in hits(src)], [(3, "binding")])

    def test_nested_consumed_helper_not_outer_tuple_result(self):
        for src in (
            "let (v, _) = consume(parse_target(i));",
            "let (v, _) = ctx.with_player_antecedents(|s| consume(parse_target(i)));",
            "let (v, rest) = parse_target(i); consume(rest);",
            "let (rest, _) = parse_target_demonstrative_subject(i)?;",
            "let (rest, _) = parse_target_anaphoric_subject(i)?;",
        ):
            self.assertEqual(hits(src), [])

    def test_lexical_and_cfg_exclusions_have_production_witness(self):
        excluded = [
            '// let (v, _) = parse_target(i);',
            '/* nested /* comment */ let (v, _) = parse_target(i); */',
            'let text = "let (v, _) = parse_target(i);";',
            'let text = r###"let (v, _) = parse_target(i); {"###;',
            'let text = br#"let (v, _) = parse_target(i);"#;',
            "let brace = '}';",
            '#[cfg(test)]\nfn hidden() { let (v, _) = parse_target(i); }',
            '#[cfg(test)]\n#[allow(dead_code)]\nmod hidden { fn f() { parse_target(i); } }',
        ]
        for src in excluded:
            with self.subTest(src=src):
                found = hits(src + "\nfn witness<'a>(i: &'a str) { let (v, _) = parse_target(i); }")
                self.assertEqual([(h.line, h.family) for h in found], [(len(src.splitlines()) + 1, "binding")])

    def test_attributed_non_items_preserve_gate_witness(self):
        for container in (
            "struct S { #[cfg(all(test, unix))] hidden: (), }",
            "enum E { #[cfg(all(test, unix))] Hidden, Visible }",
            "fn f() { match x { #[cfg(all(test, unix))] Hidden => (), Visible => () } }",
            "fn f() { #[cfg(all(test, unix))] let hidden = (); }",
        ):
            with self.subTest(container=container):
                found = hits(container + "\nfn witness() { let (v, _) = parse_target(i); }")
                self.assertEqual([(h.line, h.family) for h in found], [(2, "binding")])

    def test_const_generic_test_impl_excludes_helper_discards(self):
        src = ("#[cfg(test)] impl Foo<{ 1 }> { fn hidden(i: &str) { let (v, _) = parse_target(i); } }\n"
               "fn witness(i: &str) { let (v, _) = parse_target(i); }")
        self.assertEqual([(h.line, h.family) for h in hits(src)], [(2, "binding")])

    def discovery_hits(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name, text in files.items():
                path = root / rd.SCOPE / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            return [(Path(h.path).relative_to(rd.SCOPE).as_posix(), h.family)
                    for path, text in rd.production_sources(root, False).items()
                    for h in rd.find_discards(path, text.splitlines())]

    def test_t_exact_test_filenames_preserve_production_sources(self):
        discard = "fn f() { let (v, _) = parse_target(i); }"
        found = self.discovery_hits({
            "mod.rs": "mod contests;\nmod latest_tests_helper;\n"
                      "#[cfg(test)]\nmod support;",
            "contests.rs": discard,
            "latest_tests_helper.rs": discard,
            "tests.rs": discard,
            "foo_tests.rs": discard,
            "support.rs": discard,
            "inner.rs": "#![cfg(test)]\n" + discard,
            "snapshots/fake.rs": discard,
            "production.rs": discard,
        })
        self.assertEqual(sorted(found), [
            ("contests.rs", "binding"),
            ("latest_tests_helper.rs", "binding"),
            ("production.rs", "binding"),
        ])

    def test_children_outlined_under_exact_test_filenames_stay_test_only(self):
        discard = "fn f() { let _parsed = parse_target(i); }"
        for owner, directory in (("tests.rs", "tests"), ("unit_tests.rs", "unit_tests")):
            with self.subTest(owner=owner):
                found = self.discovery_hits({
                    "mod.rs": "#[cfg(test)]\nmod " + owner[:-3] + ";\nmod witness;",
                    owner: "mod support;",
                    directory + "/support.rs": "mod deeper;\n" + discard,
                    directory + "/support/deeper.rs": discard,
                    "witness.rs": discard,
                })
                self.assertEqual(sorted(found), [("witness.rs", "whole-result")])

    def test_production_reference_wins_over_children_of_exact_test_filenames(self):
        discard = "fn f() { let _parsed = parse_target(i); }"
        found = self.discovery_hits({
            "mod.rs": '#[cfg(test)]\nmod tests;\n#[path = "tests/support.rs"]\nmod shared;',
            "tests.rs": "mod support;",
            "tests/support.rs": discard,
        })
        self.assertEqual(found, [("tests/support.rs", "whole-result")])

    def test_production_module_reference_wins_over_test_path_alias(self):
        found = self.discovery_hits({
            "mod.rs": 'mod x;\n#[cfg(test)]\n#[path = "x.rs"]\nmod y;\nmod witness;',
            "x.rs": "let (v, _) = parse_target(input);",
            "witness.rs": "let (v, _) = parse_target(input);",
        })
        self.assertEqual(sorted(found), [("witness.rs", "binding"), ("x.rs", "binding")])

    def test_outlined_test_child_uses_enclosing_inline_module_directory(self):
        found = self.discovery_hits({
            "mod.rs": "mod outer { #[cfg(test)] mod support; }\nmod witness;",
            "outer/support.rs": "let (v, _) = parse_target(hidden);",
            "support.rs": "let (v, _) = parse_target(production);",
            "witness.rs": "let (v, _) = parse_target(production);",
        })
        self.assertEqual(sorted(found), [("support.rs", "binding"), ("witness.rs", "binding")])

    def test_discovery_excludes_test_files_not_production_witness(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parser = root / rd.SCOPE
            parser.mkdir(parents=True)
            files = {
                "mod.rs": '#[cfg(test)]\nmod support;\n#[cfg(test)]\n#[path = "odd.rs"]\nmod custom;\n#[cfg(test)]\nmod fixtures;\n#[cfg(any(test, unix))]\nmod retained;\n',
                "support.rs": "let (v, _) = parse_target(i);",
                "odd.rs": "let (v, _) = parse_target(i);",
                "fixtures/mod.rs": "mod child;",
                "fixtures/child.rs": "let (v, _) = parse_target(i);",
                "test_support.rs": "#![cfg(test)]\nlet (v, _) = parse_target(i);",
                "unit_tests.rs": "let (v, _) = parse_target(i);",
                "snapshots/fake.rs": "let (v, _) = parse_target(i);",
                "retained.rs": "let (v, _) = parse_target(i);",
                "production.rs": "let (v, _) = parse_target(i);",
            }
            for name, text in files.items():
                path = parser / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            sources = rd.production_sources(root, False)
            found = [h for path, text in sources.items() for h in rd.find_discards(path, text.splitlines())]
            self.assertEqual(sorted(Path(h.path).name for h in found), ["production.rs", "retained.rs"])


class DispositionTests(unittest.TestCase):
    REASON = "crate::parser::parsed::ResidueWaiver::Unaudited"
    FAMILY = ("parse_type_phrase_folding", "parse_type_phrase_folding_with_ctx",
              "read_type_union_leg", "fold_type_union", "parse_filter_phrase_union",
              "parse_mass_type_union")

    def violations(self, src):
        return rd.find_disposition_violations("crates/engine/src/parser/probe.rs", src)

    def test_g1_complete_family_registration(self):
        for helper in self.FAMILY:
            for expr in (f"{helper}(i)", f"crate::target::{helper}(i)",
                         f"ctx.with_player_scope(scope, |s| {{ {helper}(i, s) }})"):
                with self.subTest(expr=expr):
                    self.assertEqual([(h.family, h.symbol) for h in hits(
                        f"let (v, _) = {expr};")], [("binding", helper)])
            with self.subTest(alias=helper):
                self.assertEqual([h.symbol for h in hits(
                    f"use crate::target::{helper} as read;\nlet (v, _) = read(i);")], [helper])
        self.assertEqual(hits("let (rest, _) = parse_target_demonstrative_subject(i)?;"), [])

    def test_g2_waivers_count_once_independent_of_receiver(self):
        for receiver in ("p", "(p)", "{ p }", "Parsed::new(f(), i)",
                         "parse_type_phrase_folding(i)"):
            for expr in (f"{receiver}.waive(\n{self.REASON},\n)",
                         f"Parsed::waive({receiver}, {self.REASON},)"):
                for template in ("EXPR;", "let _ = EXPR;", "drop(EXPR);", "let v = EXPR;"):
                    src = template.replace("EXPR", expr)
                    with self.subTest(src=src):
                        self.assertEqual([(h.family, h.symbol) for h in hits(src)],
                                         [("unaudited-waiver", "waive")])
                        self.assertEqual(self.violations(src), [])
        mixed = ("let (v, _) = parse_target(i); x.unwrap_or(TargetFilter::Any);"
                 f"p.waive({self.REASON});")
        self.assertCountEqual([h.family for h in hits(mixed)],
                              ["binding", "default", "unaudited-waiver"])
        self.assertEqual(hits('let waive = 1; // p.waive(reason)\n'
                              'let text = "p.waive(reason)";'), [])

    def test_g2_invalid_reasons_and_indirect_dispositions_fail_closed(self):
        for expr in ("p.waive(reason)", "p.waive(Unaudited)",
                     "p.waive(Reason::Unaudited)", "p.waive(ResidueWaiver::Other)",
                     "Parsed::waive(p, reason)", "<Parsed as Trait>::waive(p, reason)",
                     "crate::Parsed::waive(p, ResidueWaiver::Unaudited)",
                     "let method = Parsed::waive;", "let method = p.with_rest;",
                     "let method = Parsed::with_rest;", "p.with_rest(unexpected)"):
            with self.subTest(expr=expr):
                self.assertTrue(self.violations(expr + ";"))
        self.assertEqual(self.violations(
            "let with_rest = 1; consume(with_rest); use thing::with_rest;"
            "let value = unrelated::Parsed; with_rest(); let range = ..with_rest();"), [])

    def test_g3_expression_tail_retention_pairs(self):
        contexts = (
            "EXPR", "(EXPR)", "{ observe(); EXPR }",
            "match mode { Legacy => EXPR, Strict => strict(i) }",
            "match mode { Legacy => { observe(); EXPR } Strict => strict(i) }",
            "if cond { EXPR } else { strict(i) }",
            "if cond { strict(i) } else { EXPR }",
            "ctx.with_player_antecedents(|s| EXPR)",
            "ctx.with_player_scope(scope, |s| { observe(); EXPR })",
            "ctx.with_player_antecedents(|s| -> (Filter, &str) { EXPR })",
            "{ match mode { Legacy => ctx.with_player_scope(scope, |s| "
            "{ if cond { EXPR } else { strict(i) } }), Strict => strict(i) } }",
        )
        for context in contexts:
            for residue in ("rest", "_", "_rest"):
                src = "let (v, " + residue + ") = " + context.replace(
                    "EXPR", "p.with_rest()") + "; consume(v);"
                with self.subTest(src=src):
                    self.assertEqual(len(self.violations(src)), 0 if residue == "rest" else 1)

    def test_g3_tuple_consumers_and_discard_siblings(self):
        legal = ("let rest = p.with_rest().1;",
                 "let (v, rest) = Parsed::with_rest(p);",
                 "let rest = (p.with_rest()).1;",
                 "let rest = { p.with_rest() }.1;",
                 "let (v, rest, ..) = p.with_rest();",
                 "if let Some((v, rest)) = Some(p.with_rest()) {}",
                 "let Some((v, rest)) = Some(p.with_rest()) else { return; };",
                 "match p.with_rest() { (A, rest) | (B, rest) if keep(rest) => (), }",
                 "assert_eq!(p.with_rest(), (v, i));",
                 "assert_eq!((v, i), p.with_rest(), message);",
                 "fn enclosing() -> (Filter, &str) { p.with_rest() }",
                 "fn enclosing() -> (Filter, &str) { return p.with_rest(); }")
        illegal = ("p.with_rest().0;", "let _ = p.with_rest();",
                   "(p.with_rest()).0;", "drop((p.with_rest()).1);",
                   "let _unused = p.with_rest();", "let (v, ..) = p.with_rest();",
                   "if let Some((v, _)) = Some(p.with_rest()) {}",
                   "let Some((v, _)) = Some(p.with_rest()) else { return; };",
                   "match p.with_rest() { (A, rest) | (B, _) if keep() => (), }",
                   "drop(p.with_rest());", "p.with_rest();",
                   "let pair = p.with_rest(); pair.0;",
                   "let pair = p.with_rest(); let (v, _) = pair;",
                   "unknown(p.with_rest());",
                   "unregistered(|s| p.with_rest());")
        for src in legal:
            with self.subTest(legal=src):
                self.assertEqual(self.violations(src), [])
        for src in illegal:
            with self.subTest(illegal=src):
                self.assertTrue(self.violations(src))

    def test_g4_parsed_origin_disposal_aliases_and_observation(self):
        declaration = ("fn parse_type_phrase_folding(i: &str) -> Parsed<'_, Filter> "
                       "{ Parsed::new(filter, i) }\n")
        for origin in ("Parsed::new(filter, i)", "parse_type_phrase_folding(i)",
                       "ctx.with_player_antecedents(|s| parse_type_phrase_folding(i))"):
            for body in ("let _ = ORIGIN;", "drop(ORIGIN);", "ORIGIN;",
                         "let p = ORIGIN;", "let p = ORIGIN; let alias = (p); drop(alias);",
                         "let p = ORIGIN; { let q = p; }",
                         "let mut p = ORIGIN; p = other;",
                         "let p = ORIGIN; let p = other;",
                         f"let p = ORIGIN; fn inner(p: Parsed<'_, Filter>) "
                         f"{{ p.waive({self.REASON}); }}"):
                src = declaration + "fn f() { " + body.replace("ORIGIN", origin) + " }"
                with self.subTest(src=src):
                    self.assertTrue(self.violations(src))
            for body in (f"let p = ORIGIN; p.waive({self.REASON});",
                         "let p = ORIGIN; let (v, rest) = p.with_rest();",
                         f"let p = ORIGIN; {{ let p = other; consume(p); }} "
                         f"p.waive({self.REASON});"):
                src = declaration + "fn f() { " + body.replace("ORIGIN", origin) + " }"
                with self.subTest(src=src):
                    self.assertEqual(self.violations(src), [])
            for body in (
                    f"let p = ORIGIN; #[cfg(debug_assertions)] "
                    f"assert!(p.remainder().is_empty()); p.waive({self.REASON});",
                    "let p = ORIGIN; let alias = p; let (v, rest) = alias.with_rest();"):
                src = declaration + "fn f() { " + body.replace("ORIGIN", origin) + " }"
                with self.subTest(refused=src):
                    self.assertIn(("unsupported-scope", "p"),
                                  [(h.family, h.symbol) for h in self.violations(src)])
        self.assertEqual(self.violations(
            "fn f() { let _ = parse_type_phrase_folding(i); }"), [])
        self.assertEqual([h.family for h in hits(
            "fn f() { let _ = parse_type_phrase_folding(i); }")], ["whole-result"])

    def test_f1_shadowed_rhs_cannot_consume_outer_origin(self):
        body = ("let p = Parsed::new(filter, input); "
                "{ let p = other; let q = p; "
                f"q.waive({self.REASON}); }} ")
        source = "fn f(other: Parsed<'_, Filter>) { " + body + "}"
        self.assertEqual([(h.family, h.symbol) for h in self.violations(source)],
                         [("parsed-disposal", "p")])
        self.assertEqual(self.violations(
            "fn f(other: Parsed<'_, Filter>) { " + body
            + f"p.waive({self.REASON}); }}"), [])

    def test_oc_shadow_initializer_consumes_previous_binding(self):
        for observation in ("", "#[cfg(debug_assertions)] assert!(p.remainder().is_empty()); "):
            with self.subTest(observation=observation):
                source = ("fn f() { let p = Parsed::new(filter, i); " + observation
                          + f"let p = p.waive({self.REASON}); consume(p); }}")
                if observation:
                    self.assertIn(("unsupported-scope", "p"),
                                  [(h.family, h.symbol) for h in self.violations(source)])
                else:
                    self.assertEqual(self.violations(source), [])
                self.assertTrue(self.violations(source.replace(
                    f"p.waive({self.REASON})", "other")))

    def test_f3_constructor_paths_and_turbofish_require_disposition(self):
        for constructor in ("Parsed::new", "crate::parser::parsed::Parsed::new",
                            "Parsed::new::<Filter>", "Parsed::<Filter>::new",
                            "crate::parser::parsed::Parsed::<Filter>::new"):
            with self.subTest(constructor=constructor):
                source = f"fn f() {{ let p = {constructor}(filter, input); }}"
                self.assertEqual([(h.family, h.symbol) for h in self.violations(source)],
                                 [("parsed-disposal", "p")])
                self.assertEqual(self.violations(source[:-1]
                    + f"p.waive({self.REASON}); }}"), [])
        source = "fn f() { let p = <Parsed<'_, Filter>>::new(filter, input); }"
        self.assertTrue(self.violations(source), "unsupported constructor must fail closed")

    def test_d4_nonplain_rebindings_refuse_instead_of_resolving_scope(self):
        for binding in (
                "let g = |p: Parsed<'_, Filter>| { p.waive(REASON); };",
                "let g = |p| p.waive(REASON);",
                "let g = match x { _ => |p| p.waive(REASON) };",
                "let s = S { g: |p| p.waive(REASON) };",
                "match x { Some(p) => { p.waive(REASON); } None => {} }",
                "for p in values { p.waive(REASON); }",
                "if let Some(p) = opt { use_it(p); }",
                "while let Some(p) = next() { use_it(p); }",
                "let Some(p) = opt else { return; }; use_it(p);",
                "fn inner(p: Parsed<'_, Filter>) { p.waive(REASON); }"):
            binding = binding.replace("REASON", self.REASON)
            for position in ("before", "after"):
                with self.subTest(binding=binding, position=position):
                    origin = "let p = Parsed::new(filter, i); "
                    body = binding + origin if position == "before" else origin + binding
                    source = "fn f() { " + body + f"p.waive({self.REASON}); }}"
                    found = self.violations(source)
                    self.assertTrue(any(h.family == "unsupported-scope" and h.symbol == "p"
                                        for h in found), found)
                    # Same construct, distinct name: the outer origin is
                    # consumed normally, not rejected by a blanket syntax ban.
                    control = source.replace(binding, binding.replace("p", "other"))
                    self.assertEqual(self.violations(control), [])
        source = (f"fn f(p: Parsed<'_, Filter>) {{ let p = Parsed::new(filter, i); "
                  f"p.waive({self.REASON}); }}")
        self.assertTrue(any(h.family == "unsupported-scope" for h in self.violations(source)))

    def test_d4_refuses_even_when_shadow_read_would_hide_disposal(self):
        for binding in (
                "let g = |p| p.waive(REASON);",
                "match x { Some(p) => { p.waive(REASON); } None => {} }",
                "for p in values { p.waive(REASON); }"):
            source = ("fn f() { let p = Parsed::new(filter, i); "
                      + binding.replace("REASON", self.REASON) + " }")
            with self.subTest(binding=binding):
                self.assertTrue(any(h.family == "unsupported-scope" and h.symbol == "p"
                                    for h in self.violations(source)), self.violations(source))
                control = source.replace("let p = Parsed::new(filter, i);", "")
                self.assertEqual(self.violations(control), [])

    def test_d4_cli_reports_scope_category_and_preserves_plain_let(self):
        source = (f"fn f() {{ let p = Parsed::new(filter, i); "
                  f"if let Some(p) = opt {{ use_it(p); }} p.waive({self.REASON}); }}")
        files = {"production.rs": "fn reach() { let (v, _) = parse_target(i); }",
                 "tests.rs": source}
        result = self.run_cli(files)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("unsupported-scope p", result.stdout + result.stderr)
        files["tests.rs"] = (f"fn f() {{ let p = Parsed::new(filter, i); "
                             f"let p = p.waive({self.REASON}); consume(p); }}")
        result = self.run_cli(files)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_b2_positive_positions_reject_other_identifier_occurrences(self):
        for statement in (
                "let g = |S { x: p }| p.waive(REASON);",
                "for S { x: p } in values { p.waive(REASON); }",
                "let ok = matches!(x, Some(p) if { p.waive(REASON); true });",
                "macro_rules! m { ($p:expr) => { $p.waive(REASON) } }",
                "consume(object.p);", "consume(module::p);",
                "consume(stringify!(p));", "consume(concat!(p));",
                "consume(concat_idents!(p));", "unknown!(p);"):
            statement = statement.replace("REASON", self.REASON)
            with self.subTest(statement=statement):
                source = "fn f() { let p = Parsed::new(filter, i); " + statement + " }"
                found = self.violations(source)
                self.assertTrue(any(h.family == "unsupported-scope" and h.symbol == "p"
                                    for h in found), found)
                control = source.replace(statement, statement.replace("p", "q"))
                self.assertEqual([(h.family, h.symbol) for h in self.violations(control)],
                                 [("parsed-disposal", "p")])
                self.assertEqual(self.violations(control[:-1]
                    + f"p.waive({self.REASON}); }}"), [])
        self.assertIn(("unsupported-scope", "p"), [(h.family, h.symbol) for h in self.violations(
            f"fn f() {{ let p = Parsed::new(filter, i); let q = p; q.waive({self.REASON}); }}")])

    def test_b3_configuration_only_consumption_refuses(self):
        for expression in (
                "cfg!(test) && { p.waive(REASON); true }",
                "!cfg!(debug_assertions) || { p.waive(REASON); true }",
                "cfg!(debug_assertions).then(|| p.waive(REASON))"):
            expression = expression.replace("REASON", self.REASON)
            with self.subTest(expression=expression):
                source = "fn f() { let p = Parsed::new(filter, i); let _ = " + expression + "; }"
                self.assertTrue(any(h.family == "unsupported-scope" and h.symbol == "p"
                                    for h in self.violations(source)), self.violations(source))
                self.assertEqual(self.violations(
                    f"fn f() {{ let p = Parsed::new(filter, i); let _ = p.waive({self.REASON}); }}"), [])

    def run_cli(self, files, mode="check"):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name, text in files.items():
                path = root / rd.SCOPE / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(text.encode("utf-8"))
            (root / "scripts").mkdir()
            (root / rd.LEDGER).write_text(f"{rd.SCOPE}/production.rs 100\n")
            # No transformer files are copied or imported.
            return subprocess.run([sys.executable, "-B", str(Path(rd.__file__)),
                                   "--" + mode, "--root", str(root)],
                                  text=True, capture_output=True)

    def test_g5_exact_original_column_and_legal_control(self):
        files = {"production.rs": "fn f() { let (v, _) = parse_target(i); }\n",
                 "tests.rs": "fn witness(p: Parsed<'_, Filter>) {\n"
                             "    /* gap */ p.with_rest().0;\n}\n"}
        for mode in ("list", "check", "baseline"):
            result = self.run_cli(files, mode)
            with self.subTest(mode=mode):
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(f"{rd.SCOPE}/tests.rs:2:17", result.stdout + result.stderr)
                self.assertIn("with-rest-bypass", result.stdout + result.stderr)
                if mode == "baseline":
                    self.assertNotIn(f"{rd.SCOPE}/production.rs 1", result.stdout)
        files["tests.rs"] = files["tests.rs"].replace(
            "p.with_rest().0;", "let rest = p.with_rest().1;")
        result = self.run_cli(files)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_g5_unicode_continuation_and_original_columns(self):
        src = ('fn f() {\r\nlet é = "p.with_rest().0"; /* gap */ p.with_rest().0;\r\n'
               'let s = "fake \\\r\np.with_rest().0"; p.with_rest().0;\r\n}')
        found = self.violations(src)
        self.assertEqual([(v.line, v.column) for v in found], [(2, 40), (4, 21)])

    def test_g5_all_code_discovery_and_waiver_exclusion(self):
        waiver = f"fn f() {{ p.waive({self.REASON}); }}\n"
        files = {"mod.rs": '#[cfg(test)]\nmod support;\n#[cfg(test)]\nmod tests;\n'
                           '#[path = "tests/shared.rs"]\nmod shared;\n',
                 "production.rs": waiver,
                 "tests.rs": "mod child;\n" + waiver,
                 "tests/child.rs": waiver,
                 "tests/shared.rs": "fn keep() {}",
                 "unit_tests.rs": waiver, "support.rs": waiver,
                 "inner.rs": "#![cfg(test)]\n" + waiver,
                 "contests.rs": "fn keep() {}"}
        result = self.run_cli(files, "list")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("TOTAL 1 obligations", result.stdout)
        for name in ("tests.rs", "tests/child.rs", "unit_tests.rs", "support.rs",
                     "inner.rs", "tests/shared.rs", "contests.rs"):
            changed = dict(files)
            changed[name] += "fn bad() { p.with_rest().0; }\n"
            result = self.run_cli(changed)
            with self.subTest(name=name):
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(f"{rd.SCOPE}/{name}:", result.stdout + result.stderr)
                self.assertIn("with-rest-bypass", result.stdout + result.stderr)

    def test_g4_snapshot_wide_parsed_declarations(self):
        result = self.run_cli({
            "production.rs": "fn f() { drop(parse_type_phrase_folding(i)); }",
            "producer.rs": "fn parse_type_phrase_folding(i: &str) -> Parsed<'_, Filter> "
                           "{ Parsed::new(filter, i) }"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("parsed-disposal", result.stdout + result.stderr)

    def test_g4_qualified_parsed_return_declaration(self):
        src = ("fn parse_mass_type_union(i: &str) -> crate::parser::parsed::Parsed<'_, Filter> "
               "{ Parsed::new(filter, i) }\nfn f() { drop(parse_mass_type_union(i)); }")
        self.assertTrue(self.violations(src))


class TinyGrammarTests(unittest.TestCase):
    REASON = "ResidueWaiver::Unaudited"

    def check_scope(self, statement, name="p"):
        source = ("fn f() { let p = Parsed::new(filter, i); " + statement
                  + f" p.waive({self.REASON}); }}")
        found = rd.find_disposition_violations("crates/engine/src/parser/probe.rs", source)
        self.assertIn(("unsupported-scope", name),
                      [(h.family, h.symbol) for h in found], source)
        control = f"fn f() {{ let p = Parsed::new(filter, i); p.waive({self.REASON}); }}"
        self.assertEqual(rd.find_disposition_violations(
            "crates/engine/src/parser/probe.rs", control), [])

    def test_r4_f1_closure_after_block(self):
        for block in ("match x { _ => {} }", "{}", "if c {}"):
            with self.subTest(block=block):
                self.check_scope("let n = observe(&p); apply(q, { " + block
                                 + f" |p| p.waive({self.REASON}) }});")

    def test_r4_f2_destructuring_assignees(self):
        for assignee, value in (("[p]", "[other]"), ("(p, k)", "(other, k + n)")):
            with self.subTest(assignee=assignee):
                self.check_scope(f"let n = observe(&p); {assignee} = {value};")

    def test_r4_f3_named_macro_label_and_width(self):
        for literal in ("{:p$}", "{:.p$}", "{:>p$}"):
            with self.subTest(literal=literal):
                self.check_scope(f'println!("{literal}", value, p = 3);')

    def test_r4_f4_cfg_block_operands(self):
        for statement in (
                "if { cfg!(test) } { p.waive(REASON); }",
                "let _ = { cfg!(test) } && p.waive(REASON);",
                "let _ = !{ cfg!(test) } || p.waive(REASON);"):
            with self.subTest(statement=statement):
                self.check_scope(statement.replace("REASON", self.REASON))

    def test_r4_f5_macro_function_discovery(self):
        self.check_scope("macro_rules! unused { () => { fn helper() { consume(p); } }; }")
        self.check_scope("unused! { fn helper() {} }")

    def test_r4_f6_cooked_escape_capture(self):
        for literal in (r'"\u{7b}p}"', r'"\x7bp}"'):
            with self.subTest(literal=literal):
                self.check_scope(f"println!({literal});")

    def test_r4_f7_raw_disposition_identifiers(self):
        for name, arguments in (("with_rest", ""), ("waive", self.REASON)):
            for origin in ("", "let p = Parsed::new(filter, i);"):
                with self.subTest(name=name, origin=origin):
                    source = f"fn f() {{ {origin} p.r#{name}({arguments}); }}"
                    found = rd.find_disposition_violations(
                        "crates/engine/src/parser/probe.rs", source)
                    self.assertTrue(any(h.family == "unsupported-scope" for h in found), found)
        self.assertEqual(rd.find_disposition_violations(
            "crates/engine/src/parser/probe.rs",
            f"fn f() {{ p.waive({self.REASON}); }}"), [])

    def test_r4_occurrence_position_battery(self):
        for statement in (
                "p = other;", "p += other;", "[p] = [other];",
                "(p, k) = (other, k);", 'println!("{}", p = 3);',
                'println!("{:p$}", value);', 'println!("{:.p$}", value);',
                "let g = |p| p;", "for p in values {}", "match x { Some(p) => (), _ => () }",
                "if let Some(p) = x {}", "let S { field: p } = x;",
                "consume(object.p);", "consume(module::p);",
                "{ consume(p); }", "let g = || consume(p);", "println!(p);",
                "#[cfg(test)] p.waive(ResidueWaiver::Unaudited);",
                "let _ = { #[cfg(test)] observe(); true } && consume(p);",
                "if { cfg!(test) } { consume(p); }", "consume(r#p);",
                r'println!("\u{7b}p}");'):
            with self.subTest(statement=statement):
                self.check_scope(statement)

    def test_visible_format_literals_only(self):
        origin = "let p = Parsed::new(filter, i);"
        for body in (
                r'scan_contains(tp.lower, "doesn\u{2019}t untap during"); ' + origin,
                origin + r' scan_contains(tp.lower, "doesn\u{2019}t untap during");',
                r'println!("\u{7b}p}"); ' + origin,
                origin + r' assert_eq!("\u{2019}", expected);',
                origin + r' println!("{}", "\u{2019}");'):
            with self.subTest(body=body):
                source = "fn f() { " + body + f"p.waive({self.REASON}); }}"
                self.assertEqual(rd.find_disposition_violations(
                    "crates/engine/src/parser/probe.rs", source), [])
        self.check_scope(r'println!("\u{7b}p}");')

    def test_test_module_configuration_is_ambient(self):
        for function_attribute in ("", "#[cfg(debug_assertions)]"):
            source = ("#[cfg(test)] mod tests { " + function_attribute
                      + f"fn f() {{ let p = Parsed::new(filter, i); p.waive({self.REASON}); }} }}")
            found = rd.find_disposition_violations("crates/engine/src/parser/probe.rs", source)
            if function_attribute:
                self.assertIn(("unsupported-scope", "p"), [(h.family, h.symbol) for h in found])
            else:
                self.assertEqual(found, [])



class RoundFiveTests(unittest.TestCase):
    REASON = "ResidueWaiver::Unaudited"
    PATH = "crates/engine/src/parser/probe.rs"

    def test_r5_ownerless_and_macro_binders_refuse(self):
        origins = (
            "let p = Parsed::new(filter, i);",
            "let p = parse_type_phrase_folding(i);",
            "let p: Parsed<'_, Filter> = obtain();",
        )
        declaration = ("fn parse_type_phrase_folding(i: &str) -> Parsed<'_, Filter> "
                       "{ Parsed::new(filter, i) }\n")
        wrappers = (
            "my_tests! { fn t() { BODY } }",
            "my_tests!(fn t() { BODY });",
            "fn t() { unknown! { BODY } }",
            "fn t() { unknown!(BODY); }",
            "const VALUE: Filter = { BODY Filter::Any };",
            "static VALUE: Filter = { BODY Filter::Any };",
            "BODY",
        )
        for origin in origins:
            for wrapper in wrappers:
                for assignment in ("", "[p] = [other];"):
                    with self.subTest(origin=origin, wrapper=wrapper, assignment=assignment):
                        body = origin + assignment + f"p.waive({self.REASON});"
                        source = declaration + wrapper.replace("BODY", body)
                        found = rd.find_disposition_violations(self.PATH, source)
                        self.assertIn(("unsupported-scope", "p"),
                                      [(h.family, h.symbol) for h in found], source)
            control = declaration + f"fn t() {{ {origin} p.waive({self.REASON}); }}"
            self.assertEqual(rd.find_disposition_violations(self.PATH, control), [])

    def test_r5_decoded_literals_independent_of_macro(self):
        for statement in (
                'tracing::debug!("{p}");', 'report!("{p:?}");',
                'unknown!("{:>p$}");', 'consume("{:.p$}");',
                'consume(r#"{p}"#);', 'println!("{\\\n        p}");',
                r'tracing::debug!("\u{7b}p}");', r'consume("\x7bp}");',
                r'unknown!("\u{2019}");', r'report!("\x61");'):
            with self.subTest(statement=statement):
                source = ("fn f() { let p = Parsed::new(filter, i); " + statement
                          + f"p.waive({self.REASON}); }}")
                found = rd.find_disposition_violations(self.PATH, source)
                self.assertIn(("unsupported-scope", "p"),
                              [(h.family, h.symbol) for h in found], source)
        for statement in (
                'consume("{{p}}");', 'unknown!("no capture");',
                r'consume(r"\u{7b}p}");',
                r'scan_contains(tp.lower, "doesn\u{2019}t untap during");'):
            with self.subTest(control=statement):
                source = ("fn f() { let p = Parsed::new(filter, i); " + statement
                          + f"p.waive({self.REASON}); }}")
                self.assertEqual(rd.find_disposition_violations(self.PATH, source), [])


class RoundSixTests(unittest.TestCase):
    PATH = "crates/engine/src/parser/probe.rs"
    REASON = "ResidueWaiver::Unaudited"

    def test_macro_return_cannot_consume_direct_origin(self):
        source = ("macro_rules! discard { (return $e:expr) => { let _ = $e; }; } "
                  "fn probe(filter: Filter, i: &str) { "
                  "discard! { return Parsed::new(filter, i) } }")
        found = rd.find_disposition_violations(self.PATH, source)
        self.assertIn("unsupported-scope", [h.family for h in found], found)
        direct = "fn probe() { let _ = Parsed::new(filter, i); }"
        self.assertIn("parsed-disposal", [
            h.family for h in rd.find_disposition_violations(self.PATH, direct)])
        waived = f"fn probe() {{ Parsed::new(filter, i).waive({self.REASON}); }}"
        self.assertEqual(rd.find_disposition_violations(self.PATH, waived), [])

    def test_macro_let_cannot_consume_extracted_tuple(self):
        for expression in ("let (v, rest) = p.with_rest();",
                           "let rest = p.with_rest().1;"):
            with self.subTest(expression=expression):
                source = "fn f() { unknown! { " + expression + " } }"
                found = rd.find_disposition_violations(self.PATH, source)
                self.assertIn("unsupported-scope", [h.family for h in found], found)
                self.assertEqual(rd.find_disposition_violations(
                    self.PATH, "fn f() { " + expression + " }"), [])

    def test_macro_origin_battery(self):
        declaration = ("fn parse_type_phrase_folding(i: &str) -> Parsed<'_, Filter> "
                       "{ Parsed::new(filter, i) }\n")
        for origin in ("Parsed::new(filter, i)", "parse_type_phrase_folding(i)"):
            for template in (
                    "unknown!(EXPR);",
                    "macro_rules! discard { () => { EXPR }; }",
                    "vec![EXPR];",
                    "matches!(EXPR, _);",
                    "crate::unknown! { return EXPR }",
                    "unknown! { fn probe() -> Parsed<'a, Filter> { EXPR } }",
                    "unknown!(EXPR.waive(ResidueWaiver::Unaudited));",
                    "unknown!(EXPR.with_rest());"):
                with self.subTest(origin=origin, template=template):
                    source = declaration + "fn f() { " + template.replace("EXPR", origin) + " }"
                    found = rd.find_disposition_violations(self.PATH, source)
                    self.assertIn("unsupported-scope", [h.family for h in found], found)
        for pattern in ("_", "p"):
            for template in ("unknown! { BODY }", "macro_rules! m { () => { BODY }; }"):
                with self.subTest(pattern=pattern, template=template):
                    body = f"let {pattern}: Parsed<'_, Filter> = obtain();"
                    source = "fn f() { " + template.replace("BODY", body) + " }"
                    found = rd.find_disposition_violations(self.PATH, source)
                    self.assertIn("unsupported-scope", [h.family for h in found], found)


class RoundSevenTests(unittest.TestCase):
    PATH = "crates/engine/src/parser/probe.rs"
    DECLARATIONS = (
        "fn parse_type_phrase_folding(i: &str) -> Parsed<'_, Filter> "
        "{ Parsed::new(filter, i) }\n"
        "fn parse_type_phrase_folding_with_ctx(i: &str, ctx: &Ctx) -> Parsed<'_, Filter> "
        "{ Parsed::new(filter, i) }\n")

    def test_raw_constructor_disposal_and_macro_origin(self):
        for constructor in ("Parsed::new", "Parsed::r#new",
                            "crate::Parsed::<Filter>::r#new"):
            for template, category in (
                    ("fn f() { let _ = EXPR; }", "parsed-disposal"),
                    ("fn f() { discard! { return EXPR } }", "unsupported-scope")):
                with self.subTest(constructor=constructor, template=template):
                    source = template.replace("EXPR", constructor + "(filter, i)")
                    found = rd.find_disposition_violations(self.PATH, source)
                    self.assertIn(category, [h.family for h in found], found)
            source = f"fn f() {{ {constructor}(filter, i).waive(ResidueWaiver::Unaudited); }}"
            self.assertEqual(rd.find_disposition_violations(self.PATH, source), [])

    def test_raw_dispositions_and_macro_names_refuse(self):
        for method, arguments in (("with_rest", ""), ("waive", "ResidueWaiver::Unaudited"),
                                  ("remainder", "")):
            source = f"fn f() {{ p.r#{method}({arguments}); }}"
            found = rd.find_disposition_violations(self.PATH, source)
            self.assertIn("unsupported-scope", [h.family for h in found], found)
        for name in ("d", "r#d"):
            source = ("fn f() { macro_rules! " + name
                      + " { () => { fn g() -> Parsed<'a, Filter> "
                      "{ return Parsed::new(filter, i); } }; } }")
            found = rd.find_disposition_violations(self.PATH, source)
            self.assertIn("unsupported-scope", [h.family for h in found], found)

    def test_nested_returns_do_not_consume_function_results(self):
        for statement in (
                "xs.iter().map(|x| { return parse_type_phrase_folding(x); }).count();",
                "let _ = ctx.with_player_antecedents(|scoped| "
                "{ return parse_type_phrase_folding_with_ctx(rest, scoped); });",
                "let _ = (move || { return Parsed::new(f, i); })();",
                "let _fut = async { return parse_type_phrase_folding(i); };",
                "let _fut = async move { return parse_type_phrase_folding(i); };",
                "let g = (|| { return parse_type_phrase_folding(i).with_rest(); })().0;",
                "let g = async move || { return parse_type_phrase_folding(i); };",
                "let g = |x: &str| -> Parsed<'_, Filter> "
                "{ if ready { return parse_type_phrase_folding(x); } unreachable!() };"):
            with self.subTest(statement=statement):
                source = self.DECLARATIONS + "fn p(i: &str) -> (A, B) { " + statement + " (a, b) }"
                found = rd.find_disposition_violations(self.PATH, source)
                self.assertIn("unsupported-scope", [h.family for h in found], found)
                tail = source.replace("return ", "").replace("; }", " }")
                self.assertTrue(rd.find_disposition_violations(self.PATH, tail), tail)
        for body in ("return parse_type_phrase_folding(i);",
                     "if ready { return parse_type_phrase_folding(i); } parse_type_phrase_folding(i)"):
            source = self.DECLARATIONS + "fn p(i: &str) -> Parsed<'_, Filter> { " + body + " }"
            self.assertEqual(rd.find_disposition_violations(self.PATH, source), [])
        source = "fn p() -> (A, B) { return p.with_rest(); }"
        self.assertEqual(rd.find_disposition_violations(self.PATH, source), [])

    def test_macro_return_cannot_consume_nonorigin_with_rest(self):
        source = "fn f() -> (A, B) { m! { return p.with_rest() } }"
        found = rd.find_disposition_violations(self.PATH, source)
        self.assertIn("with-rest-bypass", [h.family for h in found], found)
        control = "fn f() -> (A, B) { return p.with_rest(); }"
        self.assertEqual(rd.find_disposition_violations(self.PATH, control), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
