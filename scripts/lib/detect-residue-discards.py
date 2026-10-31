#!/usr/bin/env python3
"""Gate H: selected helper-result discard syntax, with reviewed per-file ceilings.

This is bounded syntactic enforcement, not linear typing or arbitrary dataflow.
Production debt combines legacy helper/default discards and explicit Unaudited
waivers. A separate all-code stream rejects disposition bypasses, including tests,
independently of ceilings. Unknown disposition syntax fails closed.
Tracked-name grammar (shared with migrate-folding-parsed.py), at the binder's
own block level only:
  G-T1: plain `let [mut] N = ORIGIN;` or `let ([mut] V, [mut] N) = F(...);`.
        Sequential/sibling plain lets keep lexical block visibility.
  G-T2a (Parsed): statement/let initializer starting `N.waive(...)`,
        `N.with_rest(...)` or `N.remainder(...)`; return/binder-block tail accepts
        only waive/with_rest. No argument, alias, nested-block or closure reads.
  G-T2b (remainder read inspection; the transformer refuses these reads): N is a
        whole top-level argument of one path/method call,
        itself a complete statement or plain-let initializer. `break(N);` and
        call-argument tuples after `>` also count as real reads. No cfg, macro,
        closure, block, array or struct within that argument list.
  G-T3: exact `#[cfg(debug_assertions)] assert_no_compound_remainder(N, E);`;
        the generated Parsed observation substitutes `N.remainder()` for N.
All other occurrences refuse. Macro trees are opaque to function discovery;
N or fn/let tokens in one refuse the enclosing function. Raw identifiers naming
N/dispositions/ResidueWaiver, implicit captures/width references in any decoded
visible literal, and cooked unicode/hex escapes in visible format-string macro
arguments refuse. cfg anywhere in a containing statement/operand/header marks
that read configuration-only (except G-T3). Ownerless/macro-contained binders
and Parsed origin expressions refuse, even without a named binder. Tokens inside
macro trees cannot consume Parsed origins; known tuple assertions remain consumers
of extracted tuples, not proof of Parsed consumption.
Real tuple-era inventory: 92 discard bindings, 486 retained bindings, one G-T3
observation (counter.rs). Tuple patterns contain identifiers with optional mut,
no ref/type-ascribed remainders.
Expression-with-block statement boundaries are shared by closure tails and
comma-free match arms. Import aliases are lexical (module boundaries do not
inherit them) and apply only to unqualified calls; qualified calls use the final
identifier's own name. Precise-capture use<...> bounds are not imports. Bare
closures are not helper results: only established result-forwarding methods unwrap them.
Local bindings shadowing imported aliases are not tracked; helper import aliases
do not occur in the current production parser tree.
Binding annotations and ref/mut modifiers do not hide ignored remainders or
whole results: a single `_`/`_name` pattern counts one whole-result obligation,
even if later used, consistently with conservative residue-binding policy.
Let-else initializers stop before the top-level else, not its diverging block.
Block initializers include postfixes before result classification; trailing `?`
preserves helper origin, but arbitrary methods and tuple projections do not.
Projection receivers include standalone bare blocks, including return/break
operands, not keyword/header bodies or struct literals; origin follows the tail.
Not modelled: a bare-block projection receiver after `&mut`, `>` or `>>`, or as
an if/while/match scrutinee (none occurs in the parser tree); parenthesize it.
Each discarding top-level or-pattern alternative counts separately, including
parenthesized alternatives: splitting an arm preserves its obligation count.
Mixed alternatives count only the discarding alternatives, not the whole arm.
Argument and wrapper-pattern splitting accepts an optional trailing comma.
Range-endpoint projections count; free `drop` calls accept explicit generic
arguments; single-dot `.drop` methods never classify their helper arguments.
Registered result wrappers and their receiver segments accept turbofish arguments,
including nested types and function arrows; free-helper generic calls are unchanged.
Semicolon-terminated bare statement blocks discard helper tails, not header
blocks or let initializers (which retain their binding classification).
Discovery excludes exactly tests.rs and *_tests.rs, snapshots, and modules
proven test-only; a filename merely containing `tests` remains production.

--list reports each path:line family symbol and scan totals; --baseline prints
sorted ledger rows (never writes); --check validates scripts/residue-ratchet.txt.
--index reads SOURCE AND LEDGER from git's index, using this executing tool.
Unexpected per-file scanner failures report the path and fail closed.
"""
from __future__ import annotations

import argparse
from bisect import bisect_right
from collections import Counter
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import sys
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from zone_authority_census import (
    CensusError, ScanState, STRING_LIT, strip_noncode, iter_production_lines, non_test_code,
)

SCOPE = "crates/engine/src/parser"
LEDGER = "scripts/residue-ratchet.txt"
# Closed folding boundary; shared with the separately invoked migration tool.
FOLDING_HELPERS = frozenset((
    "parse_type_phrase_folding", "parse_type_phrase_folding_with_ctx",
    "read_type_union_leg", "fold_type_union", "parse_filter_phrase_union",
    "parse_mass_type_union",
))
HELPERS = FOLDING_HELPERS | frozenset((
    "parse_target", "parse_target_with_ctx", "parse_target_with_syntax",
    "parse_target_disjunction", "parse_target_with_disjunctive_restriction",
    "parse_fight_target",
))
# ParseContext methods verified to return `f(...)` unchanged (context.rs).
# Values are total argument counts; only the last argument is the closure.
RESULT_WRAPPERS = {"with_player_antecedents": 1, "with_player_scope": 2}
EXPRESSION_PREFIXES = frozenset(("return", "break"))
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|[0-9]+|::|=>|[^\s]")


class Hit(NamedTuple):
    path: str
    line: int
    family: str
    symbol: str

class Violation(NamedTuple):
    path: str
    line: int
    column: int
    family: str
    symbol: str


def all_code_stream(source):
    """Original-source character coordinates, including CRLF and test code."""
    state = ScanState()
    codes = []
    for line in source.splitlines(keepends=True):
        code, state = strip_noncode(line, state, preserve_columns=True)
        codes.append(code)
    if state != ScanState():
        raise CensusError("unterminated comment or literal")
    return "".join(codes)



def production_stream(relpath, lines):
    codes = [""] * len(lines)
    for i, _raw, code, _fn in iter_production_lines(relpath, lines, skip_cfg_test_items=True):
        codes[i] = code
    return "\n".join(codes)


def follows_expression_prefix(values, start):
    """Return/break take operands; a break label is not a block owner or callee."""
    if start > 0 and values[start - 1] in EXPRESSION_PREFIXES:
        return True
    return (start >= 3 and values[start - 3] == "break"
            and values[start - 2] == "'"
            and re.fullmatch(r"\w+", values[start - 1]) is not None)


def decoded_literal(literal, raw):
    """Decode Rust string escapes; raw string contents are already their value."""
    quote = literal.index('"')
    if raw:
        hashes = len(literal) - len(literal.rstrip("#"))
        return literal[quote + 1:len(literal) - hashes - 1]
    content = literal[quote + 1:-1]
    escapes = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", "0": "\0",
               "'": "'", '"': '"'}

    def decode(match):
        escape = match[0][1:]
        if escape.startswith(("\n", "\r\n")):
            return ""
        if escape.startswith("x"):
            return chr(int(escape[1:], 16))
        if escape.startswith("u{"):
            return chr(int(escape[2:-1].replace("_", ""), 16))
        return escapes[escape]

    return re.sub(r"\\(?:\r?\n[ \t\r\n]*|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)",
                  decode, content, flags=re.DOTALL)


class Expressions:
    """Balanced token views; follow result-preserving wrappers, not nested args."""
    def __init__(self, source):
        matches = list(TOKEN.finditer(source))
        self.offsets = [m.start() for m in matches]
        newlines = [m.start() for m in re.finditer("\n", source)]
        self.lines = [bisect_right(newlines, m.start()) + 1 for m in matches]
        self.values = [m.group() for m in matches]
        self.pairs = {}
        stack = []
        for i, value in enumerate(self.values):
            if value in ("(", "[", "{"):
                stack.append(i)
            elif value in (")", "]", "}"):
                if not stack or self.values[stack[-1]] != {")": "(", "]": "[", "}": "{"}[value]:
                    raise CensusError("unbalanced expression delimiters")
                opened = stack.pop()
                self.pairs[opened] = i
                self.pairs[i] = opened
        if stack:
            raise CensusError("unclosed expression delimiters")
        # Imports belong to lexical scopes, not the whole file. A module starts
        # a fresh name environment; ordinary blocks inherit their parent scope.
        v = self.values
        module_bodies = {i + 2 for i in range(len(v) - 2)
                         if v[i] == "mod" and re.fullmatch(r"\w+", v[i + 1])
                         and v[i + 2] == "{"}
        self.scopes = []
        self.parents = {-1: None}
        self.aliases = {-1: {}}
        scopes = [-1]
        for i, value in enumerate(v):
            self.scopes.append(scopes[-1])
            if value == "{":
                self.parents[i] = None if i in module_bodies else scopes[-1]
                self.aliases[i] = {}
                scopes.append(i)
            elif value == "}":
                scopes.pop()
        for i, value in enumerate(v):
            if value != "use" or i + 1 == len(v) or v[i + 1] == "<":
                continue  # `use<'a>` is a precise-capture bound, not an item.
            before = i - 1
            if before >= 0 and v[before] == ")":
                before = self.pairs[before] - 1
            if before >= 0 and v[before] == "pub":
                before -= 1
            if before >= 0 and v[before] not in (";", "{", "}", "]"):
                continue
            end = self.end(i + 1, {";"})
            if end == len(v):
                continue
            for j in range(i + 1, end - 2):
                if v[j + 1] == "as":
                    self.aliases[self.scopes[i]][v[j + 2]] = (
                        v[j] if v[j] in HELPERS else None)

    def macro_token_trees(self):
        """Macro calls/definitions are opaque, including function-shaped tokens."""
        if not hasattr(self, "_macro_trees"):
            v = self.values
            self._macro_trees = []
            i = 0
            while i < len(v):
                if v[i] == "!":
                    opened = i + 1
                    if i and v[i - 1] == "macro_rules":
                        opened += 1  # macro_rules! name { ... }
                    if opened in self.pairs and v[opened] in ("(", "[", "{"):
                        self._macro_trees.append((opened, self.pairs[opened]))
                        i = self.pairs[opened]
                i += 1
        return self._macro_trees

    def functions(self):
        """Discover real functions, never definitions inside macro token trees."""
        if not hasattr(self, "_functions"):
            v = self.values
            macros = self.macro_token_trees()
            self._functions = []
            for i, token in enumerate(v):
                if token != "fn" or any(a < i < b for a, b in macros):
                    continue
                body = self.end(i + 1, {"{", ";"})
                if v[body:body + 1] == ["{"]:
                    self._functions.append((i, body, self.pairs[body]))
        return self._functions

    def local_positions(self):
        """Construct G-T1/T2a/T2b/T3 positions; never parse general expressions."""
        if hasattr(self, "_local_positions"):
            return self._local_positions
        v = self.values
        plain_lets, parsed, remainder, configured, observations = {}, set(), set(), set(), set()
        macros = self.macro_token_trees()
        opaque = {i for a, b in macros for i in range(a, b + 1)}

        def cfg(a, b):
            return any(v[i:i + 2] == ["cfg", "!"]
                       or v[i:i + 3] in (["#", "[", "cfg"], ["#", "[", "cfg_attr"])
                       for i in range(a, b))

        def body_start(a, b):
            if v[a:a + 1] == ["{"]:
                return a
            if v[a:a + 1] not in (["if"], ["while"], ["for"], ["match"], ["loop"]):
                return b
            opened = self.body_brace(a, b)
            # A block-valued header precedes the actual body block.
            while opened < b and v[self.pairs[opened] + 1:self.pairs[opened] + 2] == ["{"]:
                opened = self.pairs[opened] + 1
            return opened

        def call(a, b):
            if a >= b or v[b - 1] != ")":
                return None
            opened = self.pairs[b - 1]
            if opened <= a or not re.fullmatch(r"[A-Za-z_]\w*", v[a]):
                return None
            if not self.postfix_receiver(a, opened):
                return None
            # One call expression, with no token-tree/pattern operands.
            for i in range(a, b):
                if v[i] in ("{", "[", "|", "!", "#"):
                    return None
                if v[i] == "(" and (i == a or not (
                        re.fullmatch(r"[A-Za-z_]\w*", v[i - 1]) or v[i - 1] in (")", ">"))):
                    return None
            return opened, self.split(opened + 1, b - 1)

        def statement(a, b, terminated, inherited=False):
            original = a
            while v[a:a + 2] == ["#", "["]:
                a = self.pairs[a + 1] + 1
            guarded = inherited or cfg(original, body_start(a, b))
            if guarded:
                configured.update(range(original, b))
            expr = a
            if v[a:a + 1] == ["let"]:
                equals = self.end(a + 1, {"=", ";"}, b)
                pattern_end = self.end(a + 1, {":"}, equals)
                pattern = v[a + 1:pattern_end]
                if (equals == b or v[equals] != "=" or not terminated
                        or self.end(equals + 1, {"else"}, b) != b):
                    return
                # Plain identifiers/tuples and existing Some/Ok wrappers only.
                if not pattern or any(not re.fullmatch(r"[A-Za-z_]\w*", x)
                                      and x not in ("(", ")", ",") for x in pattern):
                    return
                if "ref" in pattern:
                    return
                for i in range(a + 1, pattern_end):
                    if re.fullmatch(r"[A-Za-z_]\w*", v[i]) and v[i] not in ("mut", "Some", "Ok"):
                        plain_lets[i] = (a, b)
                expr = equals + 1
            returning = v[expr:expr + 1] == ["return"]
            if returning:
                expr += 1
            if expr >= b:
                return
            # G-T2a: left-most Parsed receiver, never an arbitrary argument.
            allowed = ("waive", "with_rest") if returning or not terminated else (
                "waive", "with_rest", "remainder")
            if (re.fullmatch(r"[A-Za-z_]\w*", v[expr])
                    and v[expr + 1:expr + 2] == ["."]
                    and v[expr + 2:expr + 3] and v[expr + 2] in allowed
                    and v[expr + 3:expr + 4] == ["("]):
                parsed.add(expr)
            result = call(expr, b)
            if result is None:
                return
            opened, args = result
            exact_debug = (terminated and v[original:a] == [
                "#", "[", "cfg", "(", "debug_assertions", ")", "]"]
                and v[expr:opened] == ["assert_no_compound_remainder"]
                and len(args) == 2 and not inherited
                and not cfg(a, b))
            if exact_debug:
                first, last = args[0]
                if last == first + 1:
                    remainder.add(first)
                    observations.add(first)
                elif v[first + 1:last] == [".", "remainder", "(", ")"]:
                    parsed.add(first)
                    observations.add(first)
            elif terminated and not returning:
                for first, last in args:
                    if last == first + 1 and re.fullmatch(r"[A-Za-z_]\w*", v[first]):
                        remainder.add(first)

        def block(a, b, inherited=False):
            while a < b:
                if v[a] == ";":
                    a += 1
                    continue
                start = a
                while v[a:a + 2] == ["#", "["]:
                    a = self.pairs[a + 1] + 1
                if a >= b:
                    return
                after = self.block_expression_end(a, b)
                opened = body_start(a, b)
                if after is not None and opened < b and opened > a:
                    after = max(after, self.pairs[opened] + 1)
                if v[a] == "fn":
                    body = self.end(a + 1, {"{", ";"}, b)
                    after = self.pairs[body] + 1 if v[body:body + 1] == ["{"] else body
                # A block-like statement followed by | starts a new closure,
                # never a binary-expression extension for this tiny grammar.
                if after is None or v[after:after + 1] in (
                        ["."], ["?"], ["("], ["+"], ["-"], ["*"], ["/"], ["%"]):
                    after = self.end(a, {";"}, b)
                # Brace macros/items need no semicolon; keep their trees opaque.
                tree = next(((x, y) for x, y in macros if a <= x < after
                             and self.scopes[x] == self.scopes[a] and v[x] == "{"), None)
                if tree is not None and v[a:a + 1] != ["let"]:
                    after = tree[1] + 1
                terminated = v[after:after + 1] == [";"]
                statement(start, after, terminated, inherited)
                guarded = inherited or cfg(start, body_start(a, after))
                i = a
                while i < after:
                    if i in opaque:
                        i = self.pairs[i] + 1 if v[i] in ("(", "[", "{") else i + 1
                    elif v[i] == "{":
                        block(i + 1, self.pairs[i], guarded)
                        i = self.pairs[i] + 1
                    else:
                        i += 1
                a = max(after + int(terminated), a + 1)

        # Compute attributed-item extents once, not once per function signature.
        guarded_items = []
        for at in range(len(v)):
            if v[at:at + 3] in (["#", "[", "cfg"], ["#", "[", "cfg_attr"]):
                item = self.pairs[at + 1] + 1
                boundary = self.end(item, {"{", ";"})
                if v[boundary:boundary + 1] == ["{"]:
                    ambient_test_module = (
                        v[at + 2:self.pairs[at + 1]] == ["cfg", "(", "test", ")"]
                        and v[boundary - 2:boundary - 1] == ["mod"])
                    if not ambient_test_module:
                        guarded_items.append((at, self.pairs[boundary]))
        functions = self.functions()
        for head, body, end in functions:
            inherited = any(a < head < b for a, b in guarded_items)
            block(body + 1, end, inherited)
        if not functions:
            block(0, len(v))
        self.remainder_positions = remainder
        self.observation_positions = observations
        self._local_positions = plain_lets, parsed, configured
        return self._local_positions

    def local_hazard(self, source, name, head, end, visible_start=None, visible_end=None):
        """Keep scope hazards function-wide; format literals follow visibility."""
        v = self.values
        if not hasattr(self, "_scope_hazards"):
            self._scope_hazards = {}
        key = name, head, end
        if key not in self._scope_hazards:
            self._scope_hazards[key] = (
                any(head <= a < end and (name in v[a + 1:b] or any(
                    token in ("fn", "let") for token in v[a + 1:b]))
                    for a, b in self.macro_token_trees())
                or any(v[at:at + 2] == ["r", "#"] and v[at + 2] in (
                    name, "with_rest", "waive", "remainder", "ResidueWaiver")
                    for at in range(head, end - 2)))
        if self._scope_hazards[key]:
            return "scope"
        if not hasattr(self, "_literals"):
            # Reuse ScanState/strip_noncode to distinguish literals from quoted
            # comment text. Regex only bounds candidates; the lexer owns validity.
            starts, states, state, offset = [], [], ScanState(), 0
            for line in source.splitlines(keepends=True):
                starts.append(offset)
                states.append(state)
                _code, state = strip_noncode(line, state, preserve_columns=True)
                offset += len(line)
            pattern = re.compile(
                r'(?<!\w)(?:br|cr|r)(?P<hashes>\#*)".*?"(?P=hashes)|'
                + STRING_LIT.pattern, re.DOTALL)
            self._literals = []
            cursor = 0
            while match := pattern.search(source, cursor):
                line = bisect_right(starts, match.start()) - 1
                prefix = source[starts[line]:match.start()] + "LEXICAL_SENTINEL"
                code, _state = strip_noncode(prefix, states[line], preserve_columns=True)
                if code.endswith("LEXICAL_SENTINEL"):
                    if not match.group().startswith("'"):
                        literal = match.group()
                        raw = match.group("hashes") is not None
                        self._literals.append((match.start(), match.end(), literal, raw,
                                               decoded_literal(literal, raw)))
                    cursor = match.end()
                else:
                    cursor = match.start() + 1
        if not hasattr(self, "_format_literals"):
            # Known expression-list macros have a specific message argument.
            # Unknown/aliased macros conservatively use their first literal.
            message_args = {
                "print": 0, "println": 0, "eprint": 0, "eprintln": 0,
                "format": 0, "format_args": 0, "format_args_nl": 0,
                "write": 1, "writeln": 1, "panic": 0, "unreachable": 0,
                "todo": 0, "unimplemented": 0, "assert": 1, "debug_assert": 1,
                "assert_eq": 2, "assert_ne": 2, "debug_assert_eq": 2,
                "debug_assert_ne": 2, "expect": 1, "expect_eq": 2, "expect_ne": 2,
            }
            self._format_literals = []
            literal_starts = [literal[0] for literal in self._literals]
            for at, token in enumerate(v):
                if token != "!" or not at:
                    continue
                opened = at + 1
                if opened not in self.pairs or v[opened] not in ("(", "[", "{"):
                    continue
                close = self.pairs[opened]
                cursor, start, argument = opened + 1, self.offsets[opened] + 1, 0
                while True:
                    comma = self.end(cursor, {","}, close)
                    if v[at - 1] not in message_args or argument == message_args[v[at - 1]]:
                        index = bisect_right(literal_starts, start - 1)
                        if index < len(self._literals) and self._literals[index][0] < self.offsets[comma]:
                            self._format_literals.append(self._literals[index])
                            break
                        if v[at - 1] in message_args:
                            break
                    if comma == close:
                        break
                    cursor, start = comma + 1, self.offsets[comma] + 1
                    argument += 1
        a = self.offsets[head if visible_start is None else visible_start]
        limit = end if visible_end is None else visible_end
        b = self.offsets[limit] if limit < len(v) else len(source)
        for start, _stop, literal, raw, _decoded in self._format_literals:
            if not a <= start < b:
                continue
            if not raw and ("\\u{" in literal or "\\x" in literal):
                return "literal"
        for start, _stop, _literal, _raw, decoded in self._literals:
            if not a <= start < b:
                continue
            for match in re.finditer(r"\{\{|\}\}|\{([^{}]*)", decoded):
                content = match[1]
                if content is not None and (re.match(r"\b" + re.escape(name) + r"\b", content)
                        or re.search(r"\b" + re.escape(name) + r"\$", content)):
                    return "literal"
        return None

    def helper_name(self, token):
        name = self.values[token]
        if token > 0 and self.values[token - 1] == "::":
            return name if name in HELPERS else None
        scope = self.scopes[token]
        while scope is not None:
            if name in self.aliases[scope]:
                return self.aliases[scope][name]
            scope = self.parents[scope]
        return name if name in HELPERS else None

    def turbofish_end(self, i):
        """Index after the `::<...>` generic-argument list starting at i, else None.

        Generic-argument commas are not expression separators, so `end` and `split`
        step over the balanced list. A turbofish is only `::` followed by `<`; a
        comparison never is, and the `>` of a `->` inside the list does not close it.
        """
        v = self.values
        if v[i] != "::" or i + 1 >= len(v) or v[i + 1] != "<":
            return None
        depth = 0
        j = i + 1
        while j < len(v):
            if v[j] in ("(", "[", "{"):
                j = self.pairs[j] + 1
                continue
            if v[j] == "<":
                depth += 1
            elif v[j] == ">" and v[j - 1] != "-":
                depth -= 1
                if depth == 0:
                    return j + 1
            j += 1
        return None

    def turbofish_start(self, end):
        """Find `::` for a balanced turbofish ending at end, else None."""
        v = self.values
        if end == 0 or v[end - 1] != ">":
            return None
        depth = 0
        i = end - 1
        while i >= 0:
            if v[i] in (")", "]", "}"):
                i = self.pairs[i] - 1
                continue
            if v[i] == ">" and (i == 0 or v[i - 1] != "-"):
                depth += 1
            elif v[i] == "<":
                depth -= 1
                if depth == 0:
                    start = i - 1
                    return (start if start >= 0 and self.turbofish_end(start) == end
                            else None)
            i -= 1
        return None

    def split(self, start, end, separator=","):
        out = []
        part = start
        i = start
        while i < end:
            if self.values[i] in ("(", "[", "{"):
                i = self.pairs[i] + 1
                continue
            after = self.turbofish_end(i)
            if after is not None:
                i = after
                continue
            if self.values[i] == separator:
                out.append((part, i))
                part = i + 1
            i += 1
        if part < end:
            out.append((part, end))
        return out

    def end(self, start, stops, limit=None):
        i = start
        limit = len(self.values) if limit is None else limit
        while i < limit:
            if self.values[i] in stops:
                break
            if self.values[i] in ("(", "[", "{"):
                i = self.pairs[i] + 1
                continue
            after = self.turbofish_end(i)
            i = i + 1 if after is None else after
        return i

    def body_brace(self, head, end):
        """The `{` opening the body of the `if`/`while`/`for`/`match` at head.

        Rust forbids struct literals in these headers, but struct PATTERNS are
        legal between `let` and `=` (and between `for` and `in`), so braces in
        that window are not the body.
        """
        v = self.values
        in_pattern = v[head] == "for"
        n = head + 1
        while n < end:
            token = v[n]
            if token in ("(", "["):
                n = self.pairs[n] + 1
                continue
            if token == "{":
                if not in_pattern:
                    return n
                n = self.pairs[n] + 1
                continue
            if token == "let":
                in_pattern = True
            elif in_pattern and token == "in" and v[head] == "for":
                in_pattern = False
            elif (in_pattern and token == "=" and v[head] != "for"
                  and v[n - 1] not in (".", "=", "!", "<", ">")
                  and n + 1 < end and v[n + 1] != "="):
                in_pattern = False
            n += 1
        return end

    def block_expression_end(self, start, end):
        """End of an expression-with-block, before any postfix/operator."""
        v = self.values
        if start >= end:
            return None
        head = start
        if v[head] == "'" and head + 3 < end and v[head + 2] == ":":
            head += 3
        if v[head] == "{":
            return self.pairs[head] + 1
        if v[head] not in ("if", "match", "unsafe", "loop", "while", "for", "async", "const"):
            return None
        opened = self.body_brace(head, end)
        if opened == end:
            return None
        after = self.pairs[opened] + 1
        if v[head] == "if" and after < end and v[after] == "else":
            return self.block_expression_end(after + 1, end)
        return after

    def block_tail(self, start, end):
        """Skip semicolon statements and comma-free expression-with-blocks."""
        while start < end:
            after = self.block_expression_end(start, end)
            if after is not None and after < end and self.values[after] not in (
                    ".", "?", "(", "[", "+", "-", "*", "/", "%", "as"):
                start = after
                continue
            stop = self.end(start, {";"}, end)
            if stop == end:
                return start, end
            start = stop + 1
        return end, end

    def match_patterns(self, start, end):
        """Arm patterns without guards; expressions-with-block need no comma."""
        v = self.values
        arm = start
        while arm < end:
            if v[arm] == ",":
                arm += 1
                continue
            arrow = self.end(arm, {"=>"}, end)
            if arrow == end:
                return
            pattern_end = self.end(arm, {"if"}, arrow)
            yield arm, pattern_end
            body = arrow + 1
            after = self.block_expression_end(body, end)
            if after is not None:
                arm = after
                # A postfix/operator makes this a larger expression, which
                # requires comma termination rather than the block shortcut.
                next_pattern = (arm < end and v[arm] in ("(", "[")
                                and self.pairs[arm] + 1 < end
                                and v[self.pairs[arm] + 1] in ("if", "=>", "|"))
                if not next_pattern and arm < end and v[arm] in (".", "?", "(", "[", "+", "-", "*", "/", "%", "as"):
                    arm = self.end(arm, {","}, end)
            else:
                arm = self.end(body, {","}, end)

    def receiver_start(self, dot):
        """Walk one postfix receiver backward without absorbing header keywords."""
        v = self.values
        start = dot
        while start > 0:
            token = v[start - 1]
            if token == "?":
                start -= 1
            elif token == ">":
                generic = self.turbofish_start(start)
                if generic is None:
                    break
                start = generic
            elif token in (")", "]"):
                start = self.pairs[start - 1]
                if (start == 0 or follows_expression_prefix(v, start)
                        or v[start - 1] in ("if", "while", "match", "for", "in")):
                    break
                if not (re.fullmatch(r"\w+", v[start - 1])
                        or v[start - 1] in (")", "]", "?")
                        or self.turbofish_start(start) is not None):
                    break
            elif re.fullmatch(r"\w+", token):
                start -= 1
                if (start > 0 and v[start - 1] in ("::", ".")
                        and not (v[start - 1] == "." and start > 1
                                 and v[start - 2] == ".")):
                    start -= 1
                else:
                    break
            else:
                break
        return start

    def postfix_receiver(self, start, end):
        """Accept one receiver chain, not a larger operator expression."""
        v = self.values
        if start < end and v[start] == "::":
            start += 1
        if start >= end:
            return False
        if re.fullmatch(r"\w+", v[start]):
            start += 1
        elif v[start] in ("(", "[") and self.pairs[start] < end:
            start = self.pairs[start] + 1
        else:
            return False
        while start < end:
            token = v[start]
            if token in ("(", "[") and self.pairs[start] < end:
                start = self.pairs[start] + 1
            elif token == "?":
                start += 1
            elif (token == "::" and re.fullmatch(r"\w+", v[start - 1])
                  and (after := self.turbofish_end(start)) is not None
                  and after <= end):
                start = after
            elif (token in ("::", ".") and start + 1 < end
                  and re.fullmatch(r"\w+", v[start + 1])):
                start += 2
            else:
                return False
        return True

    def wrapper_origin(self, name, opened, end):
        """Follow the registered wrapper's final closure result."""
        body = self.wrapper_body(name, opened, end)
        return self.origin(*body) if body else None

    def origin(self, start, end):
        """Return (helper token index, symbol) only for a helper RESULT.

        Registered context wrappers forward closure tails, including blocks
        preceded by an explicit return type.
        """
        v = self.values
        while start < end and v[end - 1] == "?":
            end -= 1
        while start < end and v[start] == "(" and self.pairs[start] == end - 1:
            start += 1
            end -= 1
        if start >= end:
            return None
        if v[start] == "{" and self.pairs[start] == end - 1:
            return self.origin(*self.block_tail(start + 1, end - 1))
        if v[end - 1] != ")":
            return None
        opened = self.pairs[end - 1]
        if opened <= start:
            return None
        generic = self.turbofish_start(opened)
        callee = opened - 1 if generic is None else generic - 1
        name = v[callee]
        if (name in RESULT_WRAPPERS and callee - 1 > start
                and v[callee - 1] == "."
                and self.postfix_receiver(start, callee - 1)):
            return self.wrapper_origin(name, opened, end)
        path = v[start:opened]
        if path and path[0] == "::":
            path = path[1:]
        if not (len(path) % 2 == 1
                and all(re.fullmatch(r"\w+", x) for x in path[::2])
                and all(x in ("::", ".") for x in path[1::2])):
            return None
        helper = self.helper_name(opened - 1)
        if helper and "." not in path:
            return opened - 1, helper
        args = self.split(opened + 1, end - 1)
        if name in ("Some", "Ok") and len(args) == 1:
            a, b = args[0]
            if a < b and v[a] not in ("|", "move"):
                return self.origin(a, b)
        return None

    def ignored_binding(self, start, end):
        """Conservative wildcard/_name policy, irrespective of later uses."""
        while start < end and self.values[start] in ("ref", "mut"):
            start += 1
        return end == start + 1 and self.values[start].startswith("_")

    def residue_pattern(self, start, end, arity=2):
        v = self.values
        while start < end:
            if v[start] == "(" and self.pairs[start] == end - 1:
                parts = self.split(start + 1, end - 1)
                rest = next((j for j, (a, b) in enumerate(parts)
                             if v[a:b] == [".", "."]), None)
                residue = 1
                if rest is not None:
                    suffix = len(parts) - rest - 1
                    if rest <= residue < arity - suffix:
                        return True
                    if residue >= arity - suffix:
                        residue = rest + 1 + residue - (arity - suffix)
                if len(parts) > residue:
                    a, b = parts[residue]
                    return self.ignored_binding(a, b)
                start += 1
                end -= 1
            elif (end > start + 2 and v[start] in ("Some", "Ok")
                  and v[start + 1] == "(" and self.pairs[start + 1] == end - 1):
                args = self.split(start + 2, end - 1)
                if len(args) != 1:
                    return False
                start, end = args[0]
            else:
                return False
        return False

    def discard_patterns(self, start, end, arity=2):
        """Yield distinct discarding alternatives, preserving their token sites."""
        alternatives = self.split(start, end, "|")
        if len(alternatives) > 1:
            for a, b in alternatives:
                if a < b:  # A leading `|` is an empty prefix, not an obligation.
                    yield from self.discard_patterns(a, b, arity)
            return
        if self.ignored_binding(start, end):
            yield start, "whole-result"
            return
        if self.residue_pattern(start, end, arity):
            yield start, "binding"
            return
        v = self.values
        if (start + 1 < end and v[start] in ("Some", "Ok")
                and v[start + 1] == "(" and self.pairs[start + 1] == end - 1):
            args = self.split(start + 2, end - 1)
            if len(args) == 1:
                yield from self.discard_patterns(*args[0], arity)
            return
        if start < end and v[start] == "(" and self.pairs[start] == end - 1:
            # Only grouping/wrappers expose whole-result alternatives. Do not
            # confuse alternatives inside the filter component with tuple arms.
            if len(self.split(start + 1, end - 1)) == 1:
                yield from self.discard_patterns(start + 1, end - 1, arity)
    def initializer_end(self, start):
        """A full let RHS, without swallowing an if-let body or let-else."""
        after = self.block_expression_end(start, len(self.values))
        return self.end(after if after is not None else start, {";", "{", "}", "else"})

    def wrapper_body(self, name, opened, end):
        """The registered closure result range, with optional return type."""
        v = self.values
        args = self.split(opened + 1, end - 1)
        count = RESULT_WRAPPERS[name]
        if len(args) < count:
            return None
        a, b = args[count - 1][0], args[-1][1]
        a += a < b and v[a] == "move"
        if a >= b or v[a] != "|":
            return None
        close = self.end(a + 1, {"|"}, b)
        if close == b:
            return None
        body = close + 1
        if v[body:body + 2] == ["-", ">"]:
            body = self.end(body + 2, {"{"}, b)
            return (body, b) if body < b and self.pairs[body] == b - 1 else None
        return (body, b) if len(args) == count else None

    def result_leaves(self, start, end, leaves):
        """Propagate bounded expression tails to their immediate consumer.

        `leaves` maps exact token ranges to caller-owned identities. Never follow
        arbitrary arguments, closures or methods as result-forwarding operations.
        """
        v = self.values
        if (start, end) in leaves:
            return {leaves[start, end]}
        if start >= end:
            return set()
        if v[end - 1] == "?":
            return self.result_leaves(start, end - 1, leaves)
        if v[start] == "(" and self.pairs[start] == end - 1:
            return self.result_leaves(start + 1, end - 1, leaves)
        if v[start] == "{" and self.pairs[start] == end - 1:
            return self.result_leaves(*self.block_tail(start + 1, end - 1), leaves)
        if v[start] == "if":
            opened = self.body_brace(start, end)
            if opened >= end:
                return set()
            close = self.pairs[opened]
            found = self.result_leaves(opened, close + 1, leaves)
            if close + 1 < end and v[close + 1] == "else":
                found |= self.result_leaves(close + 2, end, leaves)
            return found
        if v[start] == "match":
            opened = self.body_brace(start, end)
            if opened >= end or self.pairs[opened] != end - 1:
                return set()
            found = set()
            for a, _b in self.match_patterns(opened + 1, end - 1):
                arrow = self.end(a, {"=>"}, end - 1)
                body = arrow + 1
                after = self.block_expression_end(body, end - 1)
                if after is None or (after < end - 1 and v[after] in (".", "?", "(")):
                    after = self.end(body, {","}, end - 1)
                found |= self.result_leaves(body, after, leaves)
            return found
        if v[end - 1] != ")":
            return set()
        opened = self.pairs[end - 1]
        if opened <= start:
            return set()
        generic = self.turbofish_start(opened)
        callee = opened - 1 if generic is None else generic - 1
        name = v[callee]
        if (name in RESULT_WRAPPERS and callee > start and v[callee - 1] == "."
                and self.postfix_receiver(start, callee - 1)):
            body = self.wrapper_body(name, opened, end)
            return self.result_leaves(*body, leaves) if body else set()
        args = self.split(opened + 1, end - 1)
        if v[start:opened] in (["Some"], ["Ok"]) and len(args) == 1:
            return self.result_leaves(*args[0], leaves)
        return set()

    def retains_residue(self, start, end):
        """Every alternative must destructure a named, non-underscore residue."""
        v = self.values
        alternatives = [(a, b) for a, b in self.split(start, end, "|") if a < b]
        if len(alternatives) > 1:
            return all(self.retains_residue(a, b) for a, b in alternatives)
        if start >= end:
            return False
        if v[start] in ("Some", "Ok") and start + 1 < end and v[start + 1] == "(":
            args = self.split(start + 2, end - 1)
            return len(args) == 1 and self.retains_residue(*args[0])
        if v[start] == "(" and self.pairs[start] == end - 1:
            parts = self.split(start + 1, end - 1)
            if len(parts) == 1:
                return self.retains_residue(*parts[0])
            residue = 1
            rest = next((j for j, (a, b) in enumerate(parts)
                         if v[a:b] == [".", "."]), None)
            if rest is not None:
                suffix = len(parts) - rest - 1
                if rest <= residue < 2 - suffix:
                    return False
                if residue >= 2 - suffix:
                    residue = rest + 1 + residue - (2 - suffix)
            if len(parts) > residue:
                a, b = parts[residue]
                while a < b and v[a] in ("ref", "mut"):
                    a += 1
                return (b == a + 1 and re.fullmatch(r"[A-Za-z]\w*", v[a]) is not None)
        return False


class Disposition(NamedTuple):
    token: int
    start: int
    end: int
    name: str
    receiver: tuple[int, int] | None
    error: str | None


def dispositions(ex):
    """Recognize method calls and the deliberately supported bare Parsed UFCS."""
    v = ex.values
    for i, name in enumerate(v):
        if name not in ("with_rest", "waive") or i == 0:
            continue
        method = v[i - 1] == "." and (i < 2 or v[i - 2] != ".")
        ufcs = v[i - 1] == "::"
        if not method and not ufcs:
            continue
        # A use item is a reference, not an extracted disposition operation.
        before = i - 1
        while before >= 0 and v[before] not in (";", "{", "}"):
            if v[before] == "use":
                break
            before -= 1
        if before >= 0 and v[before] == "use":
            continue
        opened = ex.turbofish_end(i + 1) if i + 1 < len(v) else None
        if opened is None:
            opened = i + 1
        if opened >= len(v) or v[opened] != "(":
            yield Disposition(i, i, i + 1, name, None, "indirect-disposition")
            continue
        end = ex.pairs[opened] + 1
        args = ex.split(opened + 1, end - 1)
        error = None
        if method:
            start = ex.receiver_start(i - 1)
            if i >= 2 and v[i - 2] == "}":
                start = ex.pairs[i - 2]
            receiver = (start, i - 1)
        else:
            start = ex.receiver_start(i + 1)
            # Only Parsed::op(value, ...) is supported, not trait/qualified UFCS.
            if v[start:i] != ["Parsed", "::"]:
                error = "unsupported-disposition-ufcs"
            receiver = args.pop(0) if args else None
        expected = 1 if name == "waive" else 0
        if len(args) != expected:
            error = "invalid-disposition-arguments"
        elif name == "waive":
            a, b = args[0]
            reason = v[a:b]
            if (len(reason) < 3 or reason[-3:] != ["ResidueWaiver", "::", "Unaudited"]
                    or len(reason) % 2 != 1
                    or any(x != "::" for x in reason[1::2])
                    or any(re.fullmatch(r"\w+", x) is None for x in reason[::2])):
                error = "unsupported-waiver-reason"
        yield Disposition(i, start, end, name, receiver, error)


def parsed_returning_helpers(sources):
    """Declaration origins from the complete selected snapshot, not debt files."""
    found = set()
    for source in sources:
        if "Parsed" not in source:
            continue  # No Parsed return type can occur without its identifier.
        ex = Expressions(all_code_stream(source))
        v = ex.values
        for i, body, _end in ex.functions():
            if v[i + 1] in FOLDING_HELPERS:
                signature = v[i + 2:body]
                arrow = next((j for j in range(len(signature) - 1)
                              if signature[j:j + 2] == ["-", ">"]), None)
                output = signature[arrow + 2:] if arrow is not None else []
                # A type path ends before generic arguments, never at a nested
                # Parsed inside another return type such as Option<Parsed<...>>.
                type_end = next((j for j, token in enumerate(output)
                                 if token != "::" and re.fullmatch(r"\w+", token) is None),
                                len(output))
                if output[:type_end] and output[type_end - 1] == "Parsed":
                    found.add(v[i + 1])
    return frozenset(found)


def find_disposition_violations(relpath, source, parsed_helpers=None):
    """All executable parser code; errors never enter ceiling accounting."""
    if not isinstance(source, str):
        source = "\n".join(source)
    stream = all_code_stream(source)
    ex = Expressions(stream)
    v = ex.values
    macro_tokens = {i for a, b in ex.macro_token_trees() for i in range(a, b + 1)}
    ops = list(dispositions(ex))
    parsed_helpers = (parsed_returning_helpers([source]) if parsed_helpers is None
                      else parsed_helpers)
    errors = {}
    newlines = [m.start() for m in re.finditer("\n", source)]

    def reject(token, category):
        offset = ex.offsets[token]
        line = bisect_right(newlines, offset)
        column = offset - (newlines[line - 1] if line else -1)
        errors.setdefault(token, Violation(relpath, line + 1, column, category, v[token]))
    for at in range(len(v) - 2):
        if v[at:at + 2] == ["r", "#"] and v[at + 2] in (
                "with_rest", "waive", "remainder", "ResidueWaiver"):
            reject(at + 2, "unsupported-scope")

    leaves = {}
    kinds = {}
    sites = {}
    consumed = set()
    receivers = []
    for op in ops:
        if op.error:
            reject(op.token, op.error)
        if op.receiver and op.token not in macro_tokens:
            receivers.append(op.receiver)
        if op.name == "with_rest" and not op.error:
            key = op.token
            end = op.end
            leaves[op.start, end] = key
            kinds[key] = "tuple"
            sites[key] = key

    # Constructor and Parsed-returning family calls establish origin. A
    # qualified unrelated enum variant named Parsed does not.
    for i, name in enumerate(v):
        opened = i + 1
        if opened >= len(v) or (i and v[i - 1] == "fn"):
            continue
        opened = ex.turbofish_end(opened) or opened
        if opened >= len(v) or v[opened] != "(":
            continue
        type_end = i - 1
        generic = ex.turbofish_start(type_end)
        type_name = generic - 1 if generic is not None else i - 2
        constructor = (name == "new" and i >= 2 and v[i - 1] == "::"
                       and type_name >= 0 and v[type_name] == "Parsed")
        if (name == "new" and i >= 2 and v[i - 1] == "::"
                and v[i - 2] == ">" and generic is None):
            # Qualified-type syntax is outside the supported path/turbofish
            # grammar. Do not let a bracketed Parsed constructor disappear.
            boundary = max((j for j in range(i) if v[j] in ("=", ";", "{", "}")),
                           default=-1)
            if "Parsed" in v[boundary + 1:i]:
                reject(i, "unsupported-parsed-constructor")
                continue
        helper = ex.helper_name(i) in parsed_helpers and (i == 0 or v[i - 1] != ".")
        if not constructor and not helper:
            continue
        start = ex.receiver_start(opened)
        end = ex.pairs[opened] + 1
        leaves[start, end] = i
        kinds[i] = "parsed"
        sites[i] = i
        if i in macro_tokens:
            reject(i, "unsupported-scope")

    # A Parsed type annotation establishes origin even for an opaque initializer.
    for i, token in enumerate(v):
        if token != "let":
            continue
        equals = ex.end(i + 1, {"=", ";", "}"})
        annotation = ex.end(i + 1, {":"}, equals)
        if (equals >= len(v) or v[equals] != "=" or annotation == equals
                or "Parsed" not in v[annotation + 1:equals]):
            continue
        if i in macro_tokens:
            reject(i + 1, "unsupported-scope")
        end = ex.initializer_end(equals + 1)
        if ex.result_leaves(equals + 1, end, leaves):
            continue
        leaves[equals + 1, end] = equals
        kinds[equals] = "parsed"
        sites[equals] = i + 1

    if not kinds:
        return sorted(errors.values())  # No origin/extraction needs local flow analysis.

    def flowing(a, b):
        return ex.result_leaves(a, b, leaves)

    # Tuple projections can have grouped/block receivers. The residue projection
    # must itself reach a named consumer; a bare `.1;` still discards it.
    for i, token in enumerate(v[:-1]):
        if token == "." and v[i + 1] in ("0", "1"):
            start = ex.pairs[i - 1] if i and v[i - 1] == "}" else ex.receiver_start(i)
            for key in flowing(start, i):
                if kinds[key] == "tuple":
                    if v[i + 1] == "0":
                        reject(sites[key], "with-rest-bypass")
                        consumed.add(key)
                    else:
                        kinds[key] = "residue"
                        leaves[start, i + 2] = key

    function_ranges = []
    for head, opened, end in ex.functions():
        signature = v[head:opened]
        tuple_output = any(signature[j:j + 3] == ["-", ">", "("]
                           for j in range(len(signature) - 2))
        function_ranges.append((opened, end, tuple_output))

    def tuple_return(token):
        owners = [(a, is_tuple) for a, b, is_tuple in function_ranges if a <= token < b]
        return bool(owners and max(owners)[1])

    def function_owner(token):
        return max((a for a, b, _is_tuple in function_ranges if a <= token < b),
                   default=None)

    def accept_or_reject(ids, allowed):
        for key in ids:
            if kinds[key] == "parsed" and sites[key] in macro_tokens:
                continue  # Macro tokens cannot establish result consumption.
            consumed.add(key)
            if not allowed(key):
                reject(sites[key], "parsed-disposal" if kinds[key] == "parsed"
                       else "with-rest-bypass")

    # Bindings carry Parsed origins within lexical scope. Tuple storage is
    # intentionally not tracked: it is already a forbidden bypass at extraction.
    binding_tokens = set()
    declarations = {}
    for i, token in enumerate(v):
        if token != "let":
            continue
        equals = ex.end(i + 1, {"=", ";", "}"})
        if equals >= len(v) or v[equals] != "=":
            continue
        end = ex.initializer_end(equals + 1)
        pattern_end = ex.end(i + 1, {":"}, equals)
        binding_tokens.update(range(i + 1, equals))
        for at in range(i + 1, pattern_end):
            if re.fullmatch(r"[A-Za-z_]\w*", v[at]):
                declarations.setdefault((v[at], ex.scopes[i]), []).append((end, at))

    # Both tools admit local reads by position, never by excluding a growing
    # inventory of Rust binding constructs.
    plain_lets, local_reads, configured_reads = (
        ex.local_positions() if "parsed" in kinds.values() else ({}, set(), set()))

    def unsupported_scope(name, token):
        owner = function_owner(token)
        if owner is None or any(a < token < b for a, b in ex.macro_token_trees()):
            return True
        end = ex.pairs[owner]
        head = next(head for head, body, _end in ex.functions() if body == owner)
        scope = ex.scopes[token]
        bindings = declarations.get((name, scope), ())
        visible_start = next(after for after, at in bindings if at == token)
        visible_end = min((after for after, at in bindings if at > token),
                          default=ex.pairs.get(scope, end))
        if ex.local_hazard(source, name, head, end, visible_start, visible_end):
            return True
        for at in range(head, end):
            if v[at] != name or at in plain_lets:
                continue
            bound = binding_at(name, at)
            if bound is not None and bound != token and bound in plain_lets:
                continue  # a different plain-let binding owns this occurrence
            if at not in local_reads or ex.scopes[at] != ex.scopes[token]:
                return True
            if (at in configured_reads and at not in ex.observation_positions
                    and visible_start <= at < visible_end):
                return True
        return token not in plain_lets

    def binding_at(name, token):
        scope = ex.scopes[token]
        while scope is not None:
            candidates = declarations.get((name, scope), ())
            active = [at for end, at in candidates if end <= token]
            if active:
                return max(active)
            scope = ex.parents[scope]
        return None

    for i, token in enumerate(v):
        if token != "let":
            continue
        equals = ex.end(i + 1, {"=", ";", "}"})
        if equals >= len(v) or v[equals] != "=":
            continue
        end = ex.initializer_end(equals + 1)
        pattern_end = ex.end(i + 1, {":"}, equals)
        a = i + 1
        while a < pattern_end and v[a] in ("mut", "ref"):
            a += 1
        named = (pattern_end == a + 1 and re.fullmatch(r"[A-Za-z]\w*", v[a]) is not None)
        ids = flowing(equals + 1, end)
        # Resolve reads before transferring ownership, not in a cleanup pass.
        for key in ids:
            if i in macro_tokens:
                reject(a if kinds[key] == "parsed" and named else sites[key],
                       "unsupported-scope")
                continue
            if kinds[key] == "parsed" and named:
                consumed.add(key)  # ownership transfers to the new local
                alias = a
                kinds[alias] = "parsed"
                sites[alias] = a
                if unsupported_scope(v[a], a):
                    reject(a, "unsupported-scope")
                    continue
                # Install only reads dominated by this binding and in its scope.
                for j in range(end, len(v)):
                    scope = ex.scopes[j]
                    while scope is not None and scope != ex.scopes[i]:
                        scope = ex.parents[scope]
                    if (scope is not None and v[j] == v[a] and j not in binding_tokens
                            and function_owner(j) == function_owner(i)
                            and binding_at(v[a], j) == alias):
                        leaves[j, j + 1] = alias
            else:
                accept_or_reject({key}, lambda k: (
                    ex.retains_residue(i + 1, pattern_end) if kinds[k] == "tuple"
                    else named and kinds[k] == "residue"))


    for a, b in receivers:
        accept_or_reject(flowing(a, b), lambda k: kinds[k] == "parsed")
    for i, token in enumerate(v):
        if i in macro_tokens:
            continue
        if token == "match":
            opened = ex.body_brace(i, len(v))
            ids = flowing(i + 1, opened)
            if opened < len(v):
                patterns = list(ex.match_patterns(opened + 1, ex.pairs[opened]))
                accept_or_reject(ids, lambda k: kinds[k] == "tuple" and all(
                    ex.retains_residue(a, b) for a, b in patterns))
        if token in ("assert_eq", "assert_ne") and v[i + 1:i + 3] == ["!", "("]:
            opened = i + 2
            for a, b in ex.split(opened + 1, ex.pairs[opened])[:2]:
                accept_or_reject(flowing(a, b), lambda k: kinds[k] != "parsed")
        if token == "return":
            end = ex.end(i + 1, {";", "}"})
            accept_or_reject(flowing(i + 1, end), lambda k: (
                kinds[k] == "parsed" or tuple_return(i)))
        if token == "fn":
            opened = ex.end(i + 1, {"{", ";"})
            if opened < len(v) and v[opened] == "{":
                tail = ex.block_tail(opened + 1, ex.pairs[opened])
                accept_or_reject(flowing(*tail), lambda k: (
                    kinds[k] == "parsed" or tuple_return(opened)))
        # Mutation of an origin local is ambiguous rather than an escape hatch.
        if token == "=" and i and i - 1 not in binding_tokens and v[i - 1] != "=":
            ids = flowing(i - 1, i)
            for key in ids:
                reject(sites[key], "ambiguous-parsed-reassignment")

    for key in kinds:
        if key not in consumed:
            reject(sites[key], "parsed-disposal" if kinds[key] == "parsed"
                   else "with-rest-bypass")
    return sorted(errors.values())



def find_discards(relpath, lines):
    """Pure classification. No filesystem, git, baseline, or mutable global state."""
    ex = Expressions(production_stream(relpath, lines))
    v = ex.values
    hits = {}

    def add(origin, family, occurrence=None):
        if origin:
            i, symbol = origin
            # Overlapping recognizers share a result occurrence. Distinct arm
            # alternatives are distinct syntactic discards of that result.
            site = i if occurrence is None else occurrence
            hits.setdefault((i, site), Hit(relpath, ex.lines[site], family, symbol))

    for i, value in enumerate(v):
        if value == "let":
            equals = ex.end(i + 1, {"=", ";", "}"})
            if equals >= len(v) or v[equals] != "=":
                continue
            end = ex.end(equals + 1, {";", "{", "}", "else"})
            # A closure/block RHS needs its opening brace, unlike if-let's body.
            if end < len(v) and v[end] == "{" and end == equals + 1:
                end = ex.pairs[end] + 1
                # Classify the whole initializer, including postfixes after the block.
                end = ex.end(end, {";", "{", "}", "else"})
            origin = ex.origin(equals + 1, end)
            arity = 3 if origin and origin[1] == "parse_target_with_syntax" else 2
            pattern_end = ex.end(i + 1, {":"}, equals)
            patterns = list(ex.discard_patterns(i + 1, pattern_end, arity))
            for site, family in patterns:
                add(origin, family, site if len(patterns) > 1 else None)
        if value == "match":
            end = ex.end(i + 1, {"{"})
            origin = ex.origin(i + 1, end)
            if origin and end < len(v):
                arity = 3 if origin[1] == "parse_target_with_syntax" else 2
                for a, b in ex.match_patterns(end + 1, ex.pairs[end]):
                    for site, family in ex.discard_patterns(a, b, arity):
                        add(origin, family, site)
        if value == "." and i + 1 < len(v) and v[i + 1] in ("0", "2"):
            # Walk backward over a standalone block or balanced call/parentheses.
            # Do not absorb an enclosing assignment or a keyword/header body.
            start = i - 1
            if start < 0:
                continue
            if v[start] == "}":
                start = ex.pairs[start]
                if (start > 0 and not follows_expression_prefix(v, start)
                        and (re.fullmatch(r"\w+", v[start - 1])
                             or v[start - 1] in (")", "]", ">"))):
                    continue
            elif v[start] == ")":
                start = ex.pairs[start]
                generic = ex.turbofish_start(start)
                callee = start - 1 if generic is None else generic - 1
                if (callee >= 0 and re.fullmatch(r"\w+", v[callee])
                        and not follows_expression_prefix(v, start)
                        and (generic is None or v[callee] in RESULT_WRAPPERS)):
                    start = callee
                    while start >= 2 and v[start - 1] in ("::", ".") and re.fullmatch(r"\w+", v[start - 2]):
                        start -= 2
                    if (v[callee] in RESULT_WRAPPERS and callee >= 2
                            and v[callee - 1] == "." and v[callee - 2] != "."):
                        start = min(start, ex.receiver_start(callee - 1))
            else:
                continue
            # A single dot leaves a method receiver; doubled dots are a range.
            if (start > 0 and v[start - 1] == "."
                    and not (start > 1 and v[start - 2] == ".")):
                continue
            origin = ex.origin(start, i)
            if origin and (v[i + 1] == "0" or origin[1] == "parse_target_with_syntax"):
                add(origin, "projection")
        if value == "drop" and i + 1 < len(v):
            opened = ex.turbofish_end(i + 1)
            # Extend free calls, not methods; doubled dots are range punctuation.
            if (i > 0 and v[i - 1] == "."
                    and not (i > 1 and v[i - 2] == ".")):
                continue
            if opened is None:
                opened = i + 1
            if opened < len(v) and v[opened] == "(":
                args = ex.split(opened + 1, ex.pairs[opened])
                if len(args) == 1:
                    add(ex.origin(*args[0]), "whole-result")
        if value == ";":
            if i > 0 and v[i - 1] == "}":
                opened = ex.pairs[i - 1]
                # Bare statement blocks only; headers and let RHSs have owners.
                if opened == 0 or v[opened - 1] in (";", "{", "}"):
                    add(ex.origin(opened, i), "whole-result")
            else:
                start = i - 1
                while start >= 0 and v[start] not in (";", "{", "}"):
                    if v[start] in (")", "]"):
                        start = ex.pairs[start]
                    start -= 1
                add(ex.origin(start + 1, i), "whole-result")
        if value == "unwrap_or" and i > 0 and v[i - 1] == "." and i + 1 < len(v) and v[i + 1] == "(":
            args = ex.split(i + 2, ex.pairs[i + 1])
            arg = v[args[0][0]:args[0][1]] if len(args) == 1 else []
            if len(arg) == 3 and arg[:2] == ["TargetFilter", "::"] and arg[2] in ("SelfRef", "Any"):
                hits[i] = Hit(relpath, ex.lines[i], "default", "TargetFilter::" + arg[2])
    for op in dispositions(ex):
        if op.name == "waive" and not op.error:
            hits[("waiver", op.token)] = Hit(relpath, ex.lines[op.token],
                                           "unaudited-waiver", "waive")
    return sorted(hits.values())


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def read_snapshot(root, path, index):
    if index:
        return git(root, "show", ":" + path).decode("utf-8")
    return (root / path).read_bytes().decode("utf-8")


def scan_file(path, operation, lines):
    """Keep internal scanner failures actionable at both per-file entrypoints."""
    try:
        return operation(path, lines)
    except CensusError as error:
        raise CensusError(f"{path}: {error}") from error
    except Exception as error:
        raise CensusError(
            f"detector internal error at {path}: {type(error).__name__}: {error}") from error


def module_edges(path, lines):
    """Outlined edges with physical paths and local production availability.

    Compare declarations at the same code offsets, not by module name. Inline
    modules contribute their directory. Attribute literals are inspected only
    after the shared lexer identifies a real `path` attribute.
    """
    state = ScanState()
    codes = []
    for line in lines:
        code, state = strip_noncode(line, state)
        codes.append(code)
    source = "\n".join(codes)
    production = "\n".join(non_test_code(path, lines))
    live_modules = {m.start() for m in re.finditer(r"\bmod\s+\w+\s*[;{]", production)}
    inner_test = bool(re.search(r"#\s*!\s*\[\s*cfg\s*\(", source) and not production.strip())
    ex = Expressions(source)
    v = ex.values
    parent = PurePosixPath(path)
    directory = parent.parent if parent.name in ("mod.rs", "lib.rs", "main.rs") else parent.with_suffix("")

    def path_attribute(mod):
        before = mod
        if before and v[before - 1] == ")":
            opened = ex.pairs[before - 1]
            if opened and v[opened - 1] == "pub":
                before = opened - 1
        elif before and v[before - 1] == "pub":
            before -= 1
        while before and v[before - 1] == "]":
            end = before - 1
            opened = ex.pairs[end]
            if not opened or v[opened - 1] != "#":
                break
            if v[opened + 1:opened + 3] == ["path", "="]:
                raw = "\n".join(lines[ex.lines[opened - 1] - 1:ex.lines[end]])
                paths = re.findall(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]', raw)
                # Ambiguous same-line path attributes stay conservative: do not
                # infer a test-only physical target from an uncertain literal.
                return paths[0] if len(paths) == 1 else None
            before = opened - 1
        return None

    edges = []

    def walk(start, end, module_dir, attr_dir, parent_live):
        n = start
        while n + 2 < end:
            if v[n] != "mod" or not re.fullmatch(r"\w+", v[n + 1]) or v[n + 2] not in (";", "{"):
                n += 1
                continue
            name = v[n + 1]
            live = parent_live and ex.offsets[n] in live_modules
            attr = path_attribute(n)
            if v[n + 2] == "{":
                close = ex.pairs[n + 2]
                child_dir = attr_dir / attr if attr is not None else module_dir / name
                walk(n + 3, close, child_dir, child_dir, live)
                n = close + 1
            else:
                targets = ([attr_dir / attr] if attr is not None else
                           [module_dir / (name + ".rs"), module_dir / name / "mod.rs"])
                edges.extend((posixpath.normpath(target.as_posix()), live) for target in targets)
                n += 3

    walk(0, len(v), directory, parent.parent, not inner_test)
    return edges, inner_test


def parser_sources(root, index):
    """One snapshot authority shared by production debt and all-code violations."""
    if index:
        paths = [p.decode() for p in git(root, "ls-files", "-z", "--", SCOPE).split(b"\0") if p]
    else:
        scope = root / SCOPE
        if not scope.is_dir():
            raise ValueError(f"missing parser root: {scope}")
        paths = [p.relative_to(root).as_posix() for p in scope.rglob("*.rs")]
    sources = {p: read_snapshot(root, p, index) for p in sorted(paths)
               if p.endswith(".rs") and "snapshots" not in PurePosixPath(p).parts}
    if not sources:
        raise ValueError("empty parser scope; cannot establish a zero-obligation baseline")
    return sources


def production_sources(root, index, sources=None):
    sources = parser_sources(root, index) if sources is None else sources

    def is_test_file(path):
        name = PurePosixPath(path).name
        return name == "tests.rs" or name.endswith("_tests.rs")

    if not any(not is_test_file(p) for p in sources):
        raise ValueError("empty parser scope; cannot establish a zero-obligation baseline")
    edges = {}
    # Exact test filenames stay in the module graph so the children they outline
    # (tests/support.rs) are test-only too; their own text is never scanned.
    test_files = {p for p in sources if is_test_file(p)}
    for path, text in sources.items():
        declared, inner_test = scan_file(path, module_edges, text.splitlines())
        edges[path] = [(target, live) for target, live in declared if target in sources]
        if inner_test:
            test_files.add(path)

    def reachable(seeds, production_only):
        seen = set(seeds)
        pending = list(seen)
        while pending:
            for target, live in edges.get(pending.pop(), ()):
                if production_only and (not live or target in test_files):
                    continue
                if target not in seen:
                    seen.add(target)
                    pending.append(target)
        return seen

    test_targets = test_files | {target for declared in edges.values() for target, live in declared if not live}
    test_reachable = reachable(test_targets, False)
    all_targets = {target for declared in edges.values() for target, _live in declared}
    # Orphan files are conservatively production. A production path to a shared
    # physical file wins over every test-only alias, including descendant edges.
    roots = (sources.keys() - test_reachable) | (sources.keys() - all_targets - test_files)
    production = reachable(roots, True)
    excluded = test_reachable - production
    return {p: text for p, text in sources.items() if p not in excluded}


def parse_ledger(text):
    ceilings = {}
    for n, raw in enumerate(text.splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        fields = raw.split()
        if len(fields) != 2 or not re.fullmatch(r"[0-9]+", fields[1]):
            raise ValueError(f"malformed ledger line {n}: {raw}")
        path = fields[0].replace("\\", "/")
        parts = path.split("/")
        if (not path.startswith(SCOPE + "/") or not path.endswith(".rs")
                or any(p in ("", ".", "..") for p in parts) or ":" in path):
            raise ValueError(f"invalid ledger path at line {n}: {path}")
        if path in ceilings:
            raise ValueError(f"duplicate ledger entry for '{path}'")
        ceilings[path] = int(fields[1])
    return ceilings


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    for mode in ("check", "list", "baseline"):
        modes.add_argument("--" + mode, action="store_true")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--index", action="store_true")
    args = parser.parse_args()
    try:
        all_sources = parser_sources(args.root, args.index)
        sources = production_sources(args.root, args.index, all_sources)
        if not sources:
            raise ValueError("empty production parser scope")
        found = [h for p, text in sources.items()
                 for h in scan_file(p, find_discards, text.splitlines())]
        counts = Counter(h.path for h in found)
        parsed_helpers = parsed_returning_helpers(all_sources.values())
        violations = [h for p, text in all_sources.items()
                      for h in scan_file(p, lambda path, src: find_disposition_violations(
                          path, src, parsed_helpers), text)]
        for h in violations:
            print(f"Gate H FAIL: {h.path}:{h.line}:{h.column}: {h.family} {h.symbol}")
        if args.baseline:
            if violations:
                return 1  # Never print a usable baseline that forgives bypasses.
            for path, count in sorted(counts.items()):
                print(f"{path} {count}")
        elif args.list:
            for h in found:
                print(f"{h.path}:{h.line}: {h.family} {h.symbol}")
            print(f"TOTAL {len(found)} obligations; files scanned {len(sources)}; "
                  f"snapshot {'index' if args.index else 'worktree'}")
            for family, count in sorted(Counter(h.family for h in found).items()):
                print(f"FAMILY {family} {count}")
            print(f"VIOLATIONS {len(violations)}; files scanned {len(all_sources)}; "
                  f"snapshot {'index' if args.index else 'worktree'}")
            if violations:
                return 1
        else:
            ceilings = parse_ledger(read_snapshot(args.root, LEDGER, args.index))
            failed = bool(violations)
            for path in sorted(counts.keys() | ceilings.keys()):
                actual, ceiling = counts[path], ceilings.get(path, 0)
                if actual > ceiling:
                    print(f"Gate H FAIL: {path}: actual {actual}, ceiling {ceiling}"
                          + (" (no ledger entry)" if path not in ceilings else ""))
                    for h in found:
                        if h.path == path:
                            print(f"  {h.path}:{h.line}: {h.family} {h.symbol}")
                    failed = True
                elif actual < ceiling:
                    print(f"residue-ratchet: tighten {path} {ceiling} -> {actual}"
                          + (" (entry can be deleted)" if actual == 0 else ""))
            if failed:
                return 1
            print(f"Gate H PASS (residue: {len(found)} mixed obligations, ceiling {sum(ceilings.values())}; "
                  f"files scanned {len(sources)}; snapshot {'index' if args.index else 'worktree'})")
    except (ValueError, OSError, CensusError, subprocess.CalledProcessError) as error:
        print(f"Gate H FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
