#!/usr/bin/env python3
"""Helper for scripts/check-verb-coverage.sh. Read-only: never writes a file.

Two modes (see docs/verb-coverage.md):

  census   every `impress_service_impl!` block under crates/*/src against the
           table in docs/verb-coverage.md
  lib      the public surface of a library crate, optionally matched against
           a service crate's `#[impress_method]` verbs (plan-koine-host H-D7)

The parser is a SOURCE HEURISTIC, not rustc: it blanks comments and string
literals, then walks braces. It does not expand macros, follow `pub use`
re-exports, or resolve names. A trait's methods are not listed separately
(the trait is the item); trait impls are skipped; items behind `pub(crate)`,
`#[cfg(test)]` and `#[doc(hidden)]` are skipped.
"""
import bisect
import os
import re
import sys

try:
    import tomllib
except ImportError:  # pragma: no cover
    tomllib = None

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"


# --------------------------------------------------------------------------
# lexing helpers


def clean(src):
    """Blank comments and string/char literals (newlines and length kept)."""
    out = list(src)
    n = len(src)
    i = 0

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if c == "/" and src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif c == "/" and src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and re.match(r'r#*"', src[i : i + 40]) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r'r(#*)"', src[i:])
            close = '"' + m.group(1)
            j = src.find(close, i + len(m.group(0)))
            j = n if j < 0 else j + len(close)
            blank(i + len(m.group(0)), max(i + len(m.group(0)), j - len(close)))
            i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i + 1, min(j, n))
            i = j + 1
        elif c == "'":
            m = re.match(r"'(\\.[^']*|[^\\'])'", src[i:])
            if m:
                blank(i + 1, i + len(m.group(0)) - 1)
                i += len(m.group(0))
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def match_close(s, i, open_c, close_c):
    """Index just past the bracket that closes the one at s[i]."""
    depth = 0
    n = len(s)
    while i < n:
        if s[i] == open_c:
            depth += 1
        elif s[i] == close_c:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


class Lines:
    def __init__(self, text):
        self.starts = [0] + [m.end() for m in re.finditer("\n", text)]

    def of(self, idx):
        return bisect.bisect_right(self.starts, idx)


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def rs_files(root):
    found = []
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in ("target", ".git"))
        for f in sorted(files):
            if f.endswith(".rs"):
                found.append(os.path.join(d, f))
    return found


def snake(name):
    s = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", s)
    return s.replace("-", "_").lower()


# --------------------------------------------------------------------------
# public-surface scan

ITEM_RE = re.compile(
    r'^(?:(?:const|async|unsafe|safe|default|extern(?:\s+"[^"]*")?)\s+)*'
    r"(fn|struct|enum|trait|union|mod|type|use|const|static|macro_rules!|impl)\b"
)


def attr_skips(attrs):
    for a in attrs:
        flat = re.sub(r"\s+", "", a)
        if "doc(hidden)" in flat or flat in ("#[cfg(test)]",) or flat.startswith("#[cfg(all(test"):
            return True
    return False


def impl_type(head):
    """Type name of an inherent impl head, or None for a trait impl."""
    rest = head[len("impl") :].lstrip()
    if rest.startswith("<"):
        rest = rest[match_close(rest, 0, "<", ">") :].lstrip()
    # a ` for ` outside any <...> makes it a trait impl
    depth = 0
    for k, ch in enumerate(rest):
        if ch == "<":
            depth += 1
        elif ch == ">":
            depth -= 1
        elif depth == 0 and re.match(r"\sfor\s", rest[k : k + 5]):
            return None
    m = re.match(r"(?:&\s*)?(" + IDENT + r"(?:::" + IDENT + r")*)", rest)
    return m.group(1).split("::")[-1] if m else None


def scan_surface(path, text):
    """-> (items, skip_mods). items: (kind, owner, name, line); owner is the
    impl'd type for methods and None otherwise."""
    s = clean(text)
    lines = Lines(text)
    items, skip_mods = [], []
    n = len(s)

    def walk(i, end, ctx, public_scope):
        while i < end:
            while i < end and s[i].isspace():
                i += 1
            if i >= end or s[i] == "}":
                return i
            attrs = []
            while s.startswith("#", i) and re.match(r"#!?\[", s[i:]):
                j = s.index("[", i)
                k = match_close(s, j, "[", "]")
                attrs.append(s[i:k])
                i = k
                while i < end and s[i].isspace():
                    i += 1
            start = i
            depth = 0
            while i < end:
                ch = s[i]
                if ch in "([":
                    depth += 1
                elif ch in ")]":
                    depth -= 1
                elif depth <= 0 and ch in "{;}":
                    break
                i += 1
            head = re.sub(r"\s+", " ", s[start:i]).strip()
            term = s[i] if i < end else ";"
            body = None
            if term == "{":
                body = (i + 1, match_close(s, i, "{", "}") - 1)
                nxt = body[1] + 1
            else:
                nxt = i + 1 if term == ";" else i
            vis = re.match(r"pub(?:\s*\(([^)]*)\))?\s+", head + " ")
            is_pub = bool(vis) and vis.group(1) is None
            rest = head[vis.end() :] if vis else head
            m = ITEM_RE.match(rest)
            kind = m.group(1) if m else None
            skipped = attr_skips(attrs)
            line = lines.of(start)
            if kind == "mod":
                name = re.match(r"mod\s+(" + IDENT + ")", rest[m.start(1) :])
                name = name.group(1) if name else "?"
                if term == ";":
                    if "#[cfg(test)]" in [re.sub(r"\s+", "", a) for a in attrs] or skipped:
                        skip_mods.append(name)
                elif body and is_pub and not skipped and name != "tests":
                    walk(body[0], body[1], ctx, True)
            elif kind == "impl" and ctx[0] == "mod":
                if not skipped and body:
                    ty = impl_type(rest[m.start(1) :])
                    if ty:
                        walk(body[0], body[1], ("impl", ty), True)
            elif kind in ("fn", "struct", "enum", "trait") and is_pub and not skipped and public_scope:
                name = re.match(r"(?:r#)?(" + IDENT + ")", rest[m.end(1) :].lstrip())
                if name:
                    if ctx[0] == "mod":
                        items.append((kind, None, name.group(1), line))
                    elif kind == "fn":
                        items.append(("fn", ctx[1], name.group(1), line))
            i = max(nxt, i + 1) if nxt <= start else nxt
        return i

    walk(0, n, ("mod", None), True)
    return items, skip_mods


def lib_surface(crate_dir):
    src = os.path.join(crate_dir, "src")
    if not os.path.isdir(src):
        raise SystemExit(f"verb-coverage: {crate_dir} has no src/ directory")
    parsed = {}
    skipped_files = set()
    for p in rs_files(src):
        rel = os.path.relpath(p, src)
        if rel == "main.rs" or rel.startswith("bin" + os.sep):
            continue
        items, skip_mods = scan_surface(p, read(p))
        parsed[p] = items
        base = os.path.dirname(p)
        stem = os.path.splitext(os.path.basename(p))[0]
        if stem not in ("lib", "mod", "main"):
            base = os.path.join(base, stem)
        for m in skip_mods:
            skipped_files.add(os.path.join(base, m + ".rs"))
            skipped_files.add(os.path.join(base, m, "mod.rs"))
    all_items = []
    for p, items in parsed.items():
        if p in skipped_files or os.path.basename(p) == "tests.rs":
            continue
        rel = os.path.relpath(p, crate_dir)
        for kind, owner, name, line in items:
            all_items.append((kind, owner, name, rel, line))
    pub_types = {n for k, o, n, _, _ in all_items if o is None and k in ("struct", "enum", "trait")}
    out, seen = [], set()
    for kind, owner, name, rel, line in all_items:
        if owner is not None and owner not in pub_types:
            continue  # a method of a type that is not pub
        key = (kind, owner, name)
        if key in seen:
            continue
        seen.add(key)
        out.append({"kind": kind, "owner": owner, "name": name,
                    "key": f"{owner}::{name}" if owner else name, "where": f"{rel}:{line}"})
    out.sort(key=lambda d: (d["where"].split(":")[0], int(d["where"].split(":")[1])))
    return out


# --------------------------------------------------------------------------
# service verbs


def service_verbs(service_dir):
    base = os.path.join(service_dir, "src")
    if not os.path.isdir(base):
        base = service_dir
    verbs = []
    for p in rs_files(base):
        text = read(p)
        s = clean(text)
        lines = Lines(text)
        for m in re.finditer(r"#\[impress_method\b", s):
            a = m.start()
            j = s.index("[", a)
            k = match_close(s, j, "[", "]")
            while True:
                mm = re.compile(r"\s*#\[").match(s, k)
                if not mm:
                    break
                k = match_close(s, mm.end() - 1, "[", "]")
            fm = re.compile(r"\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(" + IDENT + ")").match(s, k)
            if not fm:
                continue
            # the doc/attr region above: back to the previous code boundary
            b = max(s.rfind(";", 0, a), s.rfind("{", 0, a), s.rfind("}", 0, a)) + 1
            region = text[b : fm.start(1)]
            covers = []
            for cm in re.finditer(r"//[/!]?\s*#\s*covers:\s*(.+)", region):
                covers += [c.strip().strip("`") for c in cm.group(1).split(",") if c.strip()]
            verbs.append({"name": fm.group(1), "covers": covers,
                          "where": f"{os.path.relpath(p, service_dir)}:{lines.of(a)}"})
    return verbs


def load_exclusions(service_dir):
    path = os.path.join(service_dir, "verb-exclusions.toml")
    if not os.path.isfile(path):
        return {}
    text = read(path)
    rows = []
    if tomllib:
        try:
            rows = tomllib.loads(text).get("exclude", [])
        except Exception as e:
            raise SystemExit(f"verb-coverage: {path}: {e}")
    out = {}
    for r in rows:
        if "item" not in r or not str(r.get("reason", "")).strip():
            raise SystemExit(f"verb-coverage: {path}: every [[exclude]] needs `item` and a non-empty `reason`")
        out[r["item"]] = r["reason"]
    return out


def cmd_lib(args):
    lib, service, report_only = args["lib"], args["service"], args["report_only"]
    items = lib_surface(lib)
    label = lambda d: f"{d['kind']} {d['key']}"
    print(f"verb-coverage --lib {lib}: public surface "
          f"(source heuristic, not rustc: top-level and inherent-impl `pub fn`/`pub struct`/`pub enum`/`pub trait`; "
          f"pub(crate), #[cfg(test)], #[doc(hidden)] and trait impls skipped; re-exports not followed)")
    if not service:
        for d in items:
            print(f"  {label(d):<48} {d['where']}")
        print(f"{len(items)} public items. Pass --service <service-crate-dir> to match them to verbs.")
        return 0
    verbs = service_verbs(service)
    excl = load_exclusions(service)
    by_name = {snake(v["name"]): v for v in verbs}
    by_cover = {}
    for v in verbs:
        for c in v["covers"]:
            by_cover.setdefault(c, v)
    covered, excluded, uncovered = [], [], []
    for d in items:
        last = d["name"]
        v = (by_cover.get(d["key"]) or by_cover.get(last)
             or by_name.get(snake(last)))
        if v:
            how = "covers" if (d["key"] in by_cover or last in by_cover) else "name"
            covered.append((d, f"verb `{v['name']}` ({how})"))
        elif d["key"] in excl:
            excluded.append((d, excl[d["key"]]))
        else:
            uncovered.append(d)
    print(f"service {service}: {len(verbs)} #[impress_method] verbs, {len(excl)} exclusions")
    print(f"covered ({len(covered)}):")
    for d, why in covered:
        print(f"  {label(d):<40} -> {why}")
    print(f"excluded ({len(excluded)}):")
    for d, why in excluded:
        print(f"  {label(d):<40} -- {why}")
    print(f"UNCOVERED ({len(uncovered)}):")
    for d in uncovered:
        print(f"  {label(d):<40} {d['where']}")
    keys = {d["key"] for d in items}
    for k in excl:
        if k not in keys:
            print(f"warning: verb-exclusions.toml excludes `{k}`, which is not in the public surface (stale?)", file=sys.stderr)
    if uncovered and not report_only:
        print(f"FAIL: {len(uncovered)} public item(s) have no verb and no exclusion; "
              f"write the verb, add `# covers: <item>` above one, or add an [[exclude]] with a reason to "
              f"{os.path.join(service, 'verb-exclusions.toml')}")
        return 1
    return 0


# --------------------------------------------------------------------------
# census (default mode)


def kebab(name):
    return snake(name).replace("_", "-")


def cmd_census(args):
    root = args["root"]
    doc = os.path.join(root, "docs", "verb-coverage.md")
    status = 0

    def fail(msg):
        nonlocal status
        print("FAIL: " + msg)
        status = 1

    if not os.path.isfile(doc):
        print(f"FAIL: {doc} does not exist")
        return 1
    dtext = read(doc)
    m = re.search(r"<!-- verb-coverage:begin -->(.*?)<!-- verb-coverage:end -->", dtext, re.S)
    if not m:
        print("FAIL: docs/verb-coverage.md has no <!-- verb-coverage:begin --> ... <!-- verb-coverage:end --> table")
        return 1
    rows = {}
    for line in m.group(1).splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if line.lstrip().startswith("| `") and len(cells) >= 3:
            svc = cells[0].strip("` ")
            rows[svc] = (cells[1].strip("` "), cells[2].strip())

    blocks = {}  # kebab service -> dict
    traits = {}  # (crate, name) -> verb count, trait seen
    crates = sorted(os.path.join(root, "crates", c) for c in os.listdir(os.path.join(root, "crates"))
                    if os.path.isdir(os.path.join(root, "crates", c, "src")))
    for cdir in crates:
        crate = os.path.basename(cdir)
        for p in rs_files(os.path.join(cdir, "src")):
            text = read(p)
            s = clean(text)
            for tm in re.finditer(r"#\[impress_service\b[^\]]*\]", s):
                tail = re.compile(r"(?:\s*#\[[^\]]*\])*\s*pub\s+trait\s+(" + IDENT + ")").match(s, tm.end())
                if tail:
                    ob = s.index("{", tail.end())
                    body = s[ob : match_close(s, ob, "{", "}")]
                    traits[(crate, tail.group(1))] = len(re.findall(r"#\[impress_method\b", body))
            for bm in re.finditer(r"\bimpress_service_impl!\s*\{", s):
                ob = bm.end() - 1
                body = s[ob : match_close(s, ob, "{", "}")]
                sm = re.search(r"\bservice\s*=\s*(" + IDENT + ")", body)
                if not sm:
                    fail(f"{os.path.relpath(p, root)}: an impress_service_impl! block names no `service = `")
                    continue
                name = sm.group(1)
                # methods = [ entry(...) ... ] at bracket depth 1, paren depth 0
                listed = None
                mm = re.search(r"\bmethods\s*=\s*\[", body)
                if mm:
                    lb = mm.end() - 1
                    arr = body[lb : match_close(body, lb, "[", "]")]
                    listed, bd, pd = 0, 0, 0
                    for im in re.finditer(r"[\[\]()]|" + IDENT + r"(?=\s*\()", arr):
                        t = im.group(0)
                        if t == "[": bd += 1
                        elif t == "]": bd -= 1
                        elif t == "(": pd += 1
                        elif t == ")": pd -= 1
                        elif bd == 1 and pd == 0:
                            listed += 1
                blocks[kebab(name)] = {
                    "name": name, "crate": crate, "file": os.path.relpath(p, root),
                    "effects": bool(re.search(r"\beffects\s*=\s*\{", body)), "listed": listed}

    for svc, b in sorted(blocks.items()):
        tcount = traits.get((b["crate"], b["name"]))
        if svc not in rows:
            fail(f"service {b['name']} ({svc}) has an impress_service_impl! block in {b['file']} but no row in docs/verb-coverage.md")
        if not b["effects"]:
            fail(f"{b['file']}: the impress_service_impl! block for {b['name']} does not declare `effects = {{ ... }}` (ADR-0036 D1)")
        if tcount is None:
            fail(f"service {b['name']}: no `#[impress_service] pub trait {b['name']}` in crate {b['crate']} to count verbs from")
        else:
            if b["listed"] is not None and b["listed"] != tcount:
                fail(f"service {b['name']}: the block lists {b['listed']} methods but the trait has {tcount} #[impress_method]")
            if svc in rows:
                crate, verbs = rows[svc]
                if crate != b["crate"]:
                    fail(f"docs/verb-coverage.md row {svc} names crate {crate}, but the block is in {b['crate']}")
                if verbs != str(tcount):
                    fail(f"docs/verb-coverage.md row {svc} says {verbs} verbs; the source has {tcount} #[impress_method]")
    for svc in rows:
        if svc not in blocks:
            fail(f"docs/verb-coverage.md has a row for {svc}, but no impress_service_impl! block under crates/*/src names it; delete the row")
    if status == 0:
        print(f"verb coverage: {len(blocks)} service(s) with a row, effects declared, verb counts match the source")
    return status


def main(argv):
    args = {"lib": None, "service": None, "report_only": False, "root": os.getcwd()}
    it = iter(argv)
    for a in it:
        if a == "--lib": args["lib"] = next(it, None)
        elif a == "--service": args["service"] = next(it, None)
        elif a == "--report-only": args["report_only"] = True
        elif a == "--root": args["root"] = next(it, args["root"])
        else:
            print(f"verb-coverage: unknown argument {a}", file=sys.stderr)
            return 2
    if args["lib"] is None and (args["service"] or args["report_only"]):
        print("verb-coverage: --service and --report-only need --lib <crate-dir>", file=sys.stderr)
        return 2
    if args["lib"] is not None:
        if not os.path.isdir(args["lib"]):
            print(f"verb-coverage: --lib {args['lib']}: not a directory", file=sys.stderr)
            return 2
        if args["service"] is not None and not os.path.isdir(args["service"]):
            print(f"verb-coverage: --service {args['service']}: not a directory", file=sys.stderr)
            return 2
        return cmd_lib(args)
    return cmd_census(args)


if __name__ == "__main__":
    try:
        code = main(sys.argv[1:])
        sys.stdout.flush()
    except BrokenPipeError:  # `| head`
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        code = 0
    sys.exit(code)
