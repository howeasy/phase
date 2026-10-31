#!/usr/bin/env python3
"""Migrate the closed folding family, with whole-tree preflight and git replay.

No Rust is written by --check or --replay. Producers are a declaration-bounded
manual partition, never an entire-file exemption. Unknown consumers are errors.
The permanent Gate H detector supplies the sole lexer/balanced-token authority.
"""
from __future__ import annotations

import argparse
from collections import Counter
import difflib
import importlib.util
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
from typing import NamedTuple

_SPEC = importlib.util.spec_from_file_location(
    "folding_residue", Path(__file__).resolve().parent / "lib/detect-residue-discards.py")
rd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(rd)
WAIVER_PATH = "crate::parser::parsed::ResidueWaiver::Unaudited"
SKIP_DIRS = frozenset((".git", "target", "node_modules", "snapshots", ".venv", "vendor"))


class Edit(NamedTuple):
    start: int
    end: int
    text: str
    kind: str


class SourcePlan(NamedTuple):
    rendered: str
    edits: tuple[Edit, ...]
    counts: Counter
    calls: int
    unsupported: tuple[str, ...]


class TreePlan(NamedTuple):
    originals: dict[str, str]
    rendered: dict[str, str]
    changed: dict[str, str]
    counts: Counter
    calls: int
    unsupported: tuple[str, ...]
    reports: tuple[str, ...]


def render_edits(source, edits):
    ordered = sorted(set(edits), key=lambda e: (e.start, e.end))
    previous = -1
    for edit in ordered:
        if edit.start < previous or edit.start < 0 or edit.end < edit.start or edit.end > len(source):
            raise ValueError("overlapping or invalid edit intervals")
        previous = edit.end
    for edit in reversed(ordered):
        source = source[:edit.start] + edit.text + source[edit.end:]
    return source


def plan_source(path, source):
    """Pure character-coordinate planning; no formatting, filesystem or writes."""
    counts = Counter()
    errors = []
    edits = []
    calls = {}
    try:
        stream = rd.all_code_stream(source)
        ex = rd.Expressions(stream)
        v, offsets = ex.values, ex.offsets

        def offset(token):
            return offsets[token] if token < len(v) else len(source)

        def text(a, b):
            return source[offset(a):offset(b - 1) + len(v[b - 1])] if a < b else ""

        def reject(token, reason):
            pos = offset(token)
            line = source.count("\n", 0, pos) + 1
            column = pos - source.rfind("\n", 0, pos)
            errors.append(f"{path}:{line}:{column}: {reason}")

        def replace(a, b, replacement, kind):
            edits.append(Edit(offset(a), offset(b - 1) + len(v[b - 1]), replacement, kind))

        # Rustdoc is not executable source. Refuse embedded migration examples
        # rather than silently treating fenced Rust as a checked caller.
        fence = False
        for line_number, line in enumerate(source.splitlines(), 1):
            doc = re.match(r"\s*(?:///|//!|\*)\s?(.*)", line)
            if not doc:
                fence = False
                continue
            prose = doc[1]
            if "```" in prose:
                fence = not fence
            elif fence and any(re.search(r"\b" + name + r"\b", prose) for name in rd.FOLDING_HELPERS):
                errors.append(f"{path}:{line_number}:1: unsupported Rustdoc family reference")

        imports = set()
        for i, token in enumerate(v):
            if token == "use" and v[i + 1:i + 2] != ["<"]:
                end = ex.end(i + 1, {";"})
                imports.update(range(i, min(end + 1, len(v))))
        functions = []
        producers = []
        for i, token in enumerate(v[:-1]):
            if token != "fn":
                continue
            body = ex.end(i + 2, {"{", ";"})
            if body < len(v) and v[body] == "{":
                end = ex.pairs[body]
                functions.append((i, body, end))
                if v[i + 1] in rd.FOLDING_HELPERS:
                    producers.append((i, end))

        def producer(token):
            return any(a <= token <= b for a, b in producers)

        def owner(token):
            return max((f for f in functions if f[1] < token < f[2]), default=None,
                       key=lambda f: f[1])

        for i, token in enumerate(v):
            name = ex.helper_name(i)
            if name not in rd.FOLDING_HELPERS:
                continue
            if i in imports:
                counts["import"] += 1
                continue
            if i and v[i - 1] == "fn":
                counts["definition"] += 1
                continue
            if producer(i):
                counts["producer-reference"] += 1
                continue
            opened = i + 1
            if opened < len(v):
                opened = ex.turbofish_end(opened) or opened
            if opened >= len(v) or v[opened] != "(" or (i and v[i - 1] == "."):
                reject(i, "unsupported family function-value or shadowing reference")
                continue
            start = ex.receiver_start(opened)
            end = ex.pairs[opened] + 1
            if any(v[j] == "let" and v[j + 1:j + 2] == [token]
                   and j < i and ex.scopes[j] == ex.scopes[i]
                   for j in range(max(0, i))):
                reject(i, "ambiguous shadowed family call")
                continue
            macro = next((a for a, b in ex.pairs.items() if a < i < b
                          and v[a] in ("(", "[", "{") and a >= 2 and v[a - 1] == "!"
                          and v[a - 2] not in ("assert_eq", "assert_ne")), None)
            if macro is not None:
                reject(i, "unsupported family call in macro capture")
                continue
            calls[i] = (start, end)

        leaves = {bounds: i for i, bounds in calls.items()}
        actions = {}

        def flowing(a, b):
            return ex.result_leaves(a, b, leaves)

        def assign(ids, action, kind):
            for key in ids:
                old = actions.get(key)
                if old is not None and old != action:
                    reject(key, "conflicting result consumers")
                elif old is None:
                    actions[key] = action
                    counts[kind] += 1

        # Existing legal dispositions are fixpoints, not a second extraction.
        for op in rd.dispositions(ex):
            ids = flowing(*op.receiver) if op.receiver else set()
            if op.error and (ids or op.name in ("waive", "with_rest")):
                reject(op.token, op.error)
            elif ids:
                assign(ids, "existing", "already-migrated")

        def tuple_value(a, b):
            """Known wrappers only; preserve or-pattern alternatives and modifiers."""
            alternatives = [(s, e) for s, e in ex.split(a, b, "|") if s < e]
            if len(alternatives) > 1:
                return " | ".join(tuple_value(s, e) for s, e in alternatives)
            if a >= b:
                raise ValueError("empty tuple pattern")
            if v[a] in ("Some", "Ok") and a + 1 < b and v[a + 1] == "(" and ex.pairs[a + 1] == b - 1:
                return v[a] + "(" + tuple_value(a + 2, b - 1) + ")"
            if v[a] == "(" and ex.pairs[a] == b - 1:
                parts = ex.split(a + 1, b - 1)
                if len(parts) == 1:
                    return tuple_value(*parts[0])
                if len(parts) != 2:
                    raise ValueError("unsupported tuple arity or rest pattern")
                (s, e), (r, t) = parts
                if any(x == "ref" for x in v[s:e]):
                    raise ValueError("unsupported borrow-sensitive value pattern")
                if not ex.ignored_binding(r, t):
                    raise ValueError("tuple residue is retained")
                return text(s, e)
            if ex.ignored_binding(a, b):
                return text(a, b)
            raise ValueError("unsupported discarding tuple pattern")

        def discarded_names(a, b):
            return {x for x in v[a:b] if x.startswith("_") and x != "_"}

        def uses(names, start, end):
            return [j for j in range(start, end) if v[j] in names]

        def binding_uses(names, declaration, start, end):
            """Resolve underscore reads, excluding later/nested shadow bindings."""
            bindings = []
            binding_tokens = set()
            for token, value in enumerate(v):
                if value != "let":
                    continue
                equals = ex.end(token + 1, {"=", ";", "}"})
                if equals >= len(v) or v[equals] != "=":
                    continue
                pattern_end = ex.end(token + 1, {":"}, equals)
                binding_tokens.update(range(token + 1, pattern_end))
                for at in range(token + 1, pattern_end):
                    if v[at] in names:
                        bindings.append((token, at, ex.scopes[token],
                                         ex.initializer_end(equals + 1)))
            # Parameter and arm patterns shadow only their body, not the
            # surrounding scope or a for-loop iterator expression.
            shadows = []
            for token, value in enumerate(v):
                pattern = body = after = None
                if value == "|" and (token == 0 or v[token - 1] in
                                      ("=", "(", ",", "move", "return")):
                    close = ex.end(token + 1, {"|"})
                    if close >= len(v):
                        continue
                    pattern, body = (token + 1, close), close + 1
                    if v[body:body + 2] == ["-", ">"]:
                        body = ex.end(body + 2, {"{"})
                    after = (ex.pairs[body] + 1 if body < len(v) and v[body] == "{"
                             else ex.end(body, {";", ","}))
                elif value == "for":
                    opened = ex.body_brace(token, len(v))
                    if opened >= len(v):
                        continue
                    pattern = (token + 1, ex.end(token + 1, {"in"}, opened))
                    body, after = opened, ex.pairs[opened] + 1
                elif value == "match":
                    opened = ex.body_brace(token, len(v))
                    if opened >= len(v):
                        continue
                    for a, b in ex.match_patterns(opened + 1, ex.pairs[opened]):
                        arrow = ex.end(b, {"=>"}, ex.pairs[opened])
                        after = ex.block_expression_end(arrow + 1, ex.pairs[opened])
                        if after is None:
                            after = ex.end(arrow + 1, {","}, ex.pairs[opened])
                        binding_tokens.update(range(a, b))
                        shadows.append((set(v[a:b]) & names, b, after))
                    continue
                if pattern is not None:
                    a, b = pattern
                    binding_tokens.update(range(a, b))
                    shadows.append((set(v[a:b]) & names, body, after))
            found = []
            for at in uses(names, start, end):
                if at in binding_tokens:
                    continue
                if any(v[at] in bound and declaration < a <= at < b for bound, a, b in shadows):
                    continue
                scope = ex.scopes[at]
                while scope is not None:
                    candidates = [b for b in bindings if b[2] == scope and b[3] <= at
                                  and v[b[1]] == v[at]]
                    if candidates:
                        if max(candidates)[0] == declaration:
                            found.append(at)
                        break
                    scope = ex.parents[scope]
            return found

        def captured_literal(names, start, end):
            """Refuse implicit format reads; doubled braces are literal text."""
            a, b = offset(start), offset(end)
            for match in re.finditer(r"\{\{|\}\}|\{([A-Za-z_]\w*)", source[a:b]):
                if (match[1] in names and stream[a + match.start()] == " "):
                    return True
            return False

        def debug_read(token):
            for at, value in enumerate(v[:token]):
                if value == "#" and v[at:at + 7] == [
                        "#", "[", "cfg", "(", "debug_assertions", ")", "]"]:
                    body = at + 7
                    after = ex.block_expression_end(body, len(v))
                    if after is None:
                        after = ex.end(body, {";"})
                    if body <= token < after:
                        return True
                if (value in ("debug_assert", "debug_assert_eq", "debug_assert_ne")
                        and v[at + 1:at + 2] == ["!"]):
                    opened = at + 2
                    if opened in ex.pairs and opened < token < ex.pairs[opened]:
                        return True
            return False

        def residue_binding(start, end):
            """Locate the single residue identifier through known tuple wrappers."""
            while start < end:
                if v[start] in ("Some", "Ok") and v[start + 1:start + 2] == ["("]:
                    if ex.pairs[start + 1] != end - 1:
                        return None
                    start, end = start + 2, end - 1
                elif v[start] == "(" and ex.pairs[start] == end - 1:
                    parts = ex.split(start + 1, end - 1)
                    if len(parts) == 1:
                        start, end = parts[0]
                        continue
                    if len(parts) != 2:
                        return None
                    a, b = parts[1]
                    while a < b and v[a] in ("ref", "mut"):
                        a += 1
                    return a if b == a + 1 and v[a].startswith("_") and v[a] != "_" else None
                else:
                    return None
            return None

        def rename_collision(name, declaration, scope_end):
            """Refuse capture by local/outer bindings or existing name references."""
            scope = ex.scopes[declaration]
            # Include nested scopes: a new name there could capture a renamed
            # read. References matter too, since the new local would shadow them.
            if name in v[scope + 1:scope_end]:
                return True
            fn = owner(declaration)
            if fn and name in v[fn[0]:fn[1]]:
                return True
            ancestors = set()
            parent = ex.parents[scope]
            while parent is not None:
                ancestors.add(parent)
                parent = ex.parents[parent]
            for token in range(declaration):
                if v[token] == "|":
                    close = ex.end(token + 1, {"|"}, declaration)
                    body = close + 1
                    if v[body:body + 2] == ["-", ">"]:
                        body = ex.end(body + 2, {"{"}, declaration)
                    if (close < declaration and body < declaration and v[body] == "{"
                            and declaration < ex.pairs[body] and name in v[token + 1:close]):
                        return True
                if v[token] == "for":
                    body = ex.body_brace(token, len(v))
                    if body < declaration < ex.pairs.get(body, -1):
                        pattern_end = ex.end(token + 1, {"in"}, body)
                        if name in v[token + 1:pattern_end]:
                            return True
                if v[token] == "match":
                    body = ex.body_brace(token, len(v))
                    if body < declaration < ex.pairs.get(body, -1):
                        for a, b in ex.match_patterns(body + 1, ex.pairs[body]):
                            arrow = ex.end(a, {"=>"}, ex.pairs[body])
                            end = ex.block_expression_end(arrow + 1, ex.pairs[body])
                            if end is None:
                                end = ex.end(arrow + 1, {","}, ex.pairs[body])
                            if arrow < declaration < end and name in v[a:b]:
                                return True
                if v[token] != "let" or ex.scopes[token] not in ancestors:
                    continue
                equals = ex.end(token + 1, {"=", ";", "}"})
                pattern_end = ex.end(token + 1, {":"}, equals)
                if name not in v[token + 1:pattern_end]:
                    continue
                # A prior sibling if-let/while-let does not export its binding.
                if token and v[token - 1] in ("if", "while"):
                    body = ex.body_brace(token - 1, len(v))
                    if body >= len(v) or not body < declaration < ex.pairs[body]:
                        continue
                return True
            return False

        def closure_types(ids):
            # A discarded typed forwarding closure must return the value, not
            # its old tuple. Retained closures keep the original return type.
            for key in ids:
                for j in range(calls[key][0]):
                    if v[j:j + 2] != ["-", ">"] or j == 0 or v[j - 1] != "|":
                        continue
                    body = ex.end(j + 2, {"{"})
                    if body < len(v) and body < key < ex.pairs[body]:
                        tail = ex.block_tail(body + 1, ex.pairs[body])
                        forwards = key in ex.result_leaves(*tail, {calls[key]: key})
                        registered = False
                        for wrapper, name in enumerate(v):
                            if name not in rd.RESULT_WRAPPERS:
                                continue
                            opened = wrapper + 1
                            opened = ex.turbofish_end(opened) or opened
                            if opened < len(v) and v[opened] == "(":
                                result = ex.wrapper_body(name, opened, ex.pairs[opened] + 1)
                                if result and result[0] == body:
                                    registered = True
                                    break
                        if not forwards or not registered:
                            reject(key, "unsupported non-forwarding typed closure")
                            continue
                        a, b = j + 2, body
                        if v[a] != "(" or ex.pairs[a] != b - 1:
                            reject(key, "unsupported forwarding closure return annotation")
                            continue
                        parts = ex.split(a + 1, b - 1)
                        if len(parts) != 2:
                            reject(key, "unsupported forwarding closure tuple type")
                        else:
                            replace(a, b, text(*parts[0]), "closure-type")

        # Projection disposition is inserted at the family leaf, inside lazy
        # fallback/context closures, never by hoisting their computation.
        for i, token in enumerate(v[:-1]):
            if token != "." or v[i + 1] not in ("0", "1"):
                continue
            start = ex.pairs[i - 1] if i and v[i - 1] == "}" else ex.receiver_start(i)
            ids = flowing(start, i)
            if not ids:
                continue
            if v[i + 1] == "0":
                if ex.origin(start, i) is None:
                    reject(i, "unsupported discard flow with nonfamily branch")
                    actions.update((key, "refused") for key in ids)
                    continue
                assign(ids, "waive", "value-projection")
                replace(i, i + 2, "", "projection")
                closure_types(ids)
            else:
                assign(ids, "retain", "residue-projection")

        for i, token in enumerate(v):
            if token != "let":
                continue
            equals = ex.end(i + 1, {"=", ";", "}"})
            if equals >= len(v) or v[equals] != "=":
                continue
            end = ex.initializer_end(equals + 1)
            ids = flowing(equals + 1, end)
            if not ids:
                continue
            pattern_end = ex.end(i + 1, {":"}, equals)
            if ex.retains_residue(i + 1, pattern_end):
                assign(ids, "retain", "retained-binding")
                continue
            # A named local holding Parsed is legal only with an explicit local
            # consumption. This also recognizes the generated debug sequence on
            # the second pass; opaque tuple storage is still refused below.
            if pattern_end == i + 2 and re.fullmatch(r"[A-Za-z]\w*", v[i + 1]):
                name = v[i + 1]
                scope_end = ex.pairs.get(ex.scopes[i], len(v))
                reads = uses({name}, end, scope_end)
                operations = [op for op in rd.dispositions(ex)
                              if op.receiver and v[op.receiver[0]:op.receiver[1]] == [name]
                              and end <= op.receiver[0] < scope_end and not op.error]
                consumed_reads = {op.receiver[0] for op in operations}
                observed_reads = {j for j in reads if v[j + 1:j + 4] == [".", "remainder", "("]}
                if len(operations) == 1 and set(reads) == consumed_reads | observed_reads:
                    assign(ids, "existing", "already-migrated-local")
                    continue
            if not list(ex.discard_patterns(i + 1, pattern_end)):
                reject(i, "unsupported opaque tuple storage or value pattern")
                continue
            if ex.origin(equals + 1, end) is None:
                reject(i, "unsupported discard flow with nonfamily branch")
                actions.update((key, "refused") for key in ids)
                continue
            residue = residue_binding(i + 1, pattern_end)
            names = {v[residue]} if residue is not None else discarded_names(i + 1, pattern_end)
            scope_end = ex.pairs.get(ex.scopes[i], len(v))
            reads = binding_uses(names, i, end, scope_end)
            if captured_literal(names, end, scope_end):
                reject(i, "unsupported implicit format-capture remainder read")
                actions.update((key, "refused") for key in ids)
                continue
            if reads:
                # Preserve the original counter-style debug-only observation.
                # Any other read makes the residue a retained continuation,
                # including a cfg observation followed by a runtime consumer.
                assertion = end + 8
                opened = assertion + 1
                assertion_end = ex.pairs.get(opened, -1) + 1
                debug_only = (
                    v[end:end + 1] == [";"]
                    and v[end + 1:end + 9] == [
                        "#", "[", "cfg", "(", "debug_assertions", ")", "]", "assert_no_compound_remainder"]
                    and len(ids) == 1 and len(names) == 1
                    and assertion_end > opened and v[assertion_end:assertion_end + 1] == [";"]
                    and all(opened < j < assertion_end for j in reads))
                if not debug_only and all(debug_read(j) for j in reads):
                    reject(i, "unsupported debug-only remainder observation")
                    actions.update((key, "refused") for key in ids)
                    continue
                if not debug_only:
                    if residue is None:
                        actions.update((key, "refused") for key in ids)
                        reject(i, "unsupported used underscore result pattern")
                        continue
                    retained_name = v[residue].lstrip("_")
                    if (re.fullmatch(r"[A-Za-z]\w*", retained_name) is None
                            or rename_collision(retained_name, i, scope_end)):
                        actions.update((key, "refused") for key in ids)
                        reject(i, f"remainder rename collision or invalid identifier: {retained_name}")
                        continue
                    for token in (residue, *reads):
                        replace(token, token + 1, retained_name, "retained-remainder-name")
                    assign(ids, "retain", "retained-underscore-binding")
                    continue
                try:
                    value = tuple_value(i + 1, pattern_end)
                except ValueError as error:
                    reject(i, str(error))
                    continue
                key = next(iter(ids))
                base = "parsed_" + value.removeprefix("mut ")
                if not re.fullmatch(r"[A-Za-z_]\w*", base):
                    reject(i, "unsupported debug value pattern")
                    continue
                fresh = base
                suffix = 2
                while fresh in v:
                    fresh = f"{base}_{suffix}"
                    suffix += 1
                assertion_source = source[offset(assertion):offset(assertion_end) + 1]
                local_edits = [Edit(offset(j) - offset(assertion), offset(j) - offset(assertion) + len(v[j]),
                                    fresh + ".remainder()", "debug-read") for j in reads]
                assertion_source = render_edits(assertion_source, local_edits)
                indent_start = source.rfind("\n", 0, offset(i)) + 1
                indent = source[indent_start:offset(i)]
                if indent.strip():
                    indent = ""
                replacement = (f"let {fresh} = {text(equals + 1, end)};"
                               + source[offset(end) + 1:offset(assertion)] + assertion_source
                               + "\n" + indent + f"let {value} = {fresh}.waive({WAIVER_PATH});")
                replace(i, assertion_end + 1, replacement, "debug-observation")
                assign(ids, "debug", "debug-observation")
                continue
            try:
                replacement = tuple_value(i + 1, pattern_end)
                replace(i + 1, pattern_end, replacement, "value-pattern")
                if pattern_end < equals:
                    a, b = pattern_end + 1, equals
                    if v[a] != "(" or ex.pairs[a] != b - 1:
                        raise ValueError("unsupported wrapped tuple annotation")
                    parts = ex.split(a + 1, b - 1)
                    if len(parts) != 2:
                        raise ValueError("unsupported tuple annotation")
                    replace(a, b, text(*parts[0]), "value-type")
                assign(ids, "waive", "discard-binding")
                closure_types(ids)
            except ValueError as error:
                reject(i, str(error))

        for i, token in enumerate(v):
            if token == "match":
                opened = ex.body_brace(i, len(v))
                ids = flowing(i + 1, opened)
                if ids and opened < len(v):
                    patterns = list(ex.match_patterns(opened + 1, ex.pairs[opened]))
                    if patterns and all(ex.retains_residue(a, b) for a, b in patterns):
                        assign(ids, "retain", "retained-match")
                    elif patterns and all(list(ex.discard_patterns(a, b)) for a, b in patterns):
                        try:
                            if ex.origin(i + 1, opened) is None:
                                raise ValueError("unsupported discard flow with nonfamily branch")
                            for a, b in patterns:
                                names = discarded_names(a, b)
                                arrow = ex.end(a, {"=>"}, ex.pairs[opened])
                                body = arrow + 1
                                after = ex.block_expression_end(body, ex.pairs[opened])
                                if after is None:
                                    after = ex.end(body, {","}, ex.pairs[opened])
                                if uses(names, b, after):
                                    raise ValueError("match guard/body uses discarded remainder")
                                replace(a, b, tuple_value(a, b), "match-pattern")
                            assign(ids, "waive", "discard-match")
                        except ValueError as error:
                            reject(i, str(error))
                    else:
                        reject(i, "unsupported mixed retaining/discarding match")
            if token in ("assert_eq", "assert_ne") and v[i + 1:i + 3] == ["!", "("]:
                opened = i + 2
                for a, b in ex.split(opened + 1, ex.pairs[opened])[:2]:
                    assign(flowing(a, b), "retain", "tuple-assertion")
            if token == "drop" and (i == 0 or v[i - 1] != "."):
                opened = i + 1
                opened = ex.turbofish_end(opened) or opened
                if opened < len(v) and v[opened] == "(":
                    args = ex.split(opened + 1, ex.pairs[opened])
                    if len(args) == 1:
                        ids = flowing(*args[0])
                        if ids and ex.origin(*args[0]) is None:
                            reject(i, "unsupported discard flow with nonfamily branch")
                            actions.update((key, "refused") for key in ids)
                        else:
                            assign(ids, "waive", "whole-disposal")
            if token == "return":
                end = ex.end(i + 1, {";", "}"})
                ids = flowing(i + 1, end)
                fn = owner(i)
                if ids and fn and any(v[j:j + 3] == ["-", ">", "("] for j in range(fn[0], fn[1])):
                    assign(ids, "retain", "tuple-return")
            if token == ";":
                start = i - 1
                while start >= 0 and v[start] not in (";", "{", "}"):
                    if v[start] in (")", "]"):
                        start = ex.pairs[start]
                    start -= 1
                ids = flowing(start + 1, i)
                if ids and ex.origin(start + 1, i) is None:
                    reject(i, "unsupported discard flow with nonfamily branch")
                    actions.update((key, "refused") for key in ids)
                else:
                    assign(ids, "waive", "whole-disposal")
        for head, opened, end in functions:
            if producer(head):
                continue
            ids = flowing(*ex.block_tail(opened + 1, end))
            if ids and any(v[j:j + 3] == ["-", ">", "("] for j in range(head, opened)):
                assign(ids, "retain", "tuple-return")

        for key, (_start, end) in calls.items():
            action = actions.get(key)
            if action is None:
                reject(key, "unsupported family result consumer")
            elif action in ("waive", "retain"):
                addition = f".waive({WAIVER_PATH})" if action == "waive" else ".with_rest()"
                pos = offset(end - 1) + len(v[end - 1])
                edits.append(Edit(pos, pos, addition, action))
        if not str(path).startswith(rd.SCOPE + "/") and (calls or counts["import"]):
            reject(next(iter(calls), 0), "external family reference outside parser scope")
        if errors:
            return SourcePlan(source, (), counts, len(calls), tuple(sorted(set(errors))))
        rendered = render_edits(source, edits)
        # Independent permanent enforcement rejects a malformed 'already migrated'
        # site rather than blessing it as an idempotent caller.
        violations = (rd.find_disposition_violations(path, rendered)
                      if str(path).startswith(rd.SCOPE + "/") else [])
        for hit in violations:
            errors.append(f"{hit.path}:{hit.line}:{hit.column}: {hit.family}")
        if errors:
            return SourcePlan(source, (), counts, len(calls), tuple(errors))
        return SourcePlan(rendered, tuple(edits), counts, len(calls), ())
    except (ValueError, rd.CensusError, IndexError, KeyError) as error:
        return SourcePlan(source, (), counts, len(calls), (f"{path}:1:1: {error}",))


def workspace_sources(root):
    """The sweep is broader than the write scope, including integration tests."""
    sources = {}
    for directory, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith("."))
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            path = Path(directory) / name
            if path.is_symlink():
                raise ValueError(f"symlink Rust source is unsupported: {path}")
            sources[path.relative_to(root).as_posix()] = path.read_bytes().decode("utf-8")
    return dict(sorted(sources.items()))


def format_source(root, path, source):
    """Pinned rustfmt through stdin, never following or writing child modules."""
    toolchain_path = root / "rust-toolchain.toml"
    if not toolchain_path.is_file():
        raise RuntimeError("missing pinned rust-toolchain.toml")
    import tomllib
    toolchain = tomllib.loads(toolchain_path.read_text())["toolchain"]["channel"]
    command = ["rustfmt", "+" + toolchain, "--edition", "2021", "--emit", "stdout",
               "--config", "skip_children=true"]
    # rustfmt rejects --config-path when no file exists. Without an explicit
    # path it searches from the original module's cwd, preserving normal scope.
    config = next((base / name for base in ((root / path).parent, *((root / path).parent.parents))
                   if base == root or root in base.parents
                   for name in ("rustfmt.toml", ".rustfmt.toml") if (base / name).is_file()), None)
    if config is not None:
        command.extend(("--config-path", str(config)))
    process = subprocess.run(command, input=source.encode("utf-8"), capture_output=True,
                             cwd=(root / path).parent)
    if process.returncode:
        raise RuntimeError(f"{path}: formatter failed: {process.stderr.decode('utf-8', errors='replace').strip()}")
    return process.stdout.decode("utf-8")


def plan_tree(sources):
    """Pure complete-snapshot planning, independent of git or the filesystem."""
    originals = dict(sorted(sources.items()))
    rendered = dict(originals)
    counts, errors, reports = Counter(), [], []
    calls = 0
    if not any(p.startswith(rd.SCOPE + "/") for p in originals):
        errors.append("empty parser scope")
    for path, source in originals.items():
        plan = plan_source(path, source)
        rendered[path] = plan.rendered
        counts.update(plan.counts)
        calls += plan.calls
        errors.extend(plan.unsupported)
        if plan.calls or plan.counts:
            reports.append(f"{path}: classified_calls={plan.calls} " + " ".join(
                f"{key}={value}" for key, value in sorted(plan.counts.items()) if value))
    if calls == 0:
        errors.append("empty classified-call scope; cannot establish migration readiness")
    changed = {} if errors else {p: s for p, s in rendered.items() if s != originals[p]}
    return TreePlan(originals, rendered, changed, counts, calls,
                    tuple(sorted(set(errors))), tuple(reports))


def prepare_tree(root):
    """CLI I/O boundary: load, plan, format and prove the full-tree fixpoint."""
    root = Path(root).resolve()
    try:
        plan = plan_tree(workspace_sources(root))
        if plan.unsupported:
            return plan
        rendered = dict(plan.rendered)
        for path, source in plan.changed.items():
            rendered[path] = format_source(root, path, source)
        again = plan_tree(rendered)
        errors = list(again.unsupported)
        errors.extend(f"{path}: rendered tree is not a complete fixpoint" for path in again.changed)
        parser_texts = [s for p, s in rendered.items() if p.startswith(rd.SCOPE + "/")]
        parsed_helpers = rd.parsed_returning_helpers(parser_texts)
        for path, source in rendered.items():
            if path.startswith(rd.SCOPE + "/"):
                errors.extend(f"{h.path}:{h.line}:{h.column}: {h.family}" for h in
                              rd.find_disposition_violations(path, source, parsed_helpers))
        changed = {} if errors else {p: s for p, s in rendered.items() if s != plan.originals[p]}
        return plan._replace(rendered=rendered, changed=changed,
                             unsupported=tuple(sorted(set(errors))))
    except (OSError, ValueError, RuntimeError, rd.CensusError) as error:
        return TreePlan({}, {}, {}, Counter(), 0, (str(error),), ())


def install(root, plan):
    """Validate the captured snapshot, then replace files with rollback on error."""
    if plan.unsupported:
        raise RuntimeError("unsupported tree; no installation")
    root = Path(root).resolve()
    if workspace_sources(root) != plan.originals:
        raise RuntimeError("source tree changed after preflight; no installation")
    pending, installed = {}, []
    try:
        for path, source in plan.changed.items():
            dest = root / path
            handle, temporary = tempfile.mkstemp(prefix=".folding-", dir=dest.parent)
            pending[path] = temporary
            with os.fdopen(handle, "wb") as stream:
                stream.write(source.encode("utf-8"))
            os.chmod(temporary, stat.S_IMODE(dest.stat().st_mode))
        # A second identity check covers concurrent edits during staging.
        if workspace_sources(root) != plan.originals:
            raise RuntimeError("source tree changed during staging")
        for path, temporary in pending.items():
            os.replace(temporary, root / path)
            installed.append(path)
    except Exception as error:
        failures = []
        for path in reversed(installed):
            try:
                (root / path).write_bytes(plan.originals[path].encode("utf-8"))
            except OSError as rollback:
                failures.append(f"{path}: {rollback}")
        detail = "; rollback failed: " + "; ".join(failures) if failures else "; originals restored"
        raise RuntimeError(f"installation failed: {error}{detail}") from error
    finally:
        for temporary in pending.values():
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass


def replay(plan):
    chunks = []
    for path, source in sorted(plan.changed.items()):
        for line in difflib.unified_diff(plan.originals[path].splitlines(keepends=True),
                                         source.splitlines(keepends=True),
                                         fromfile="a/" + path, tofile="b/" + path):
            chunks.append(line)
            if not line.endswith("\n"):
                chunks.append("\n\\ No newline at end of file\n")
    return "".join(chunks)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    modes = parser.add_mutually_exclusive_group(required=True)
    for mode in ("check", "apply", "replay"):
        modes.add_argument("--" + mode, action="store_true")
    args = parser.parse_args()
    plan = prepare_tree(args.root)
    for report in plan.reports:
        print(report, file=sys.stderr)
    print(f"classified_calls={plan.calls} changed_files={len(plan.changed)} unsupported={len(plan.unsupported)} "
          + " ".join(f"{k}={n}" for k, n in sorted(plan.counts.items()) if n), file=sys.stderr)
    for error in plan.unsupported:
        print(error, file=sys.stderr)
    if plan.unsupported:
        return 2
    if args.check:
        return int(bool(plan.changed))
    if args.replay:
        sys.stdout.write(replay(plan))
        return 0
    try:
        install(args.root, plan)
    except (OSError, RuntimeError) as error:
        print(error, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
