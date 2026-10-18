---
name: regex-lab
description: Test, explain and apply regular expressions (Rust regex syntax) with regex-lab's verbs, and keep a library of named patterns. Use when a person asks to find, extract, validate, replace or split text by pattern, or to understand or debug a regex.
---

# regex-lab

Every verb is `regex-lab-service_<verb>`. The matching verbs take the
pattern as `pattern` (the regex) or `saved` (a saved pattern's name), never
both, plus `flags`: letters `i` case-insensitive, `m` multi-line, `s` dot
matches newline, `x` ignore whitespace, `U` swap greed, `R` CRLF, `a`
ASCII-only classes. The syntax is the Rust `regex` crate's: no look-around
and no back-references, and every match runs in linear time.

1. **Check before running.** `check {"pattern": ...}` says whether it
   compiles, its groups, and its shortest and longest match. A bad pattern is
   refused `invalid-pattern` with `at` (line and column in the pattern) and
   `diagram`; fix that place and check again. `explain` lists the pattern's
   parts in words: read it back to the person when they ask what a regex does.
2. **Ask the smallest question.** `is-match` (does it match), `find` (the
   first match), `find-all` (every match, `limit` default 1000; `truncated`
   says there were more), `captures` / `captures-all` (groups; `records` has
   one row per match keyed by group name), `replace` (`$1`, `${name}`, or
   `literal: true`), `split`, `set-matches` (which of several patterns match,
   in one pass). Offsets are bytes; `line` and `column` are 1-based. Pass
   text inline (up to 16 MiB). A refused call answers only `ok: false`,
   `code` and `message`: never read its other fields as results.
3. **Save what will be reused.** `save-pattern {"name", "pattern", "flags",
   "description", "sample"}` refuses a pattern that does not match its
   sample, so give one. `patterns` lists them; `delete-pattern` removes one
   (destructive: ask the person first).

From a shell: `regex-lab-host call find-all --pattern '\d+' --haystack '"a1 b2"'`
(a value that parses as JSON is JSON, so quote a string of digits).

The pages: **Regex tester** runs every verb on one pattern and text as they
are typed; **Pattern library** lists the saved ones and tries a selected one.
