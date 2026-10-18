#!/usr/bin/env bash
# Delete the object files no binary in target/<profile> still needs.
#
# On macOS `split-debuginfo = "unpacked"` (the default) keeps a test or bin
# binary's debug info in its object files, next to it in deps/, and every
# rebuild writes new ones under new names; nothing ever removes the old.
# Measured 2026-10-18: ~12,000 more per workspace rebuild, 461,602 after a
# few days. A directory that size made every rustc invocation slow: after an
# edit to impress-service-core the test targets took 79.6 s to rebuild, and
# 19.4 s once it was cleaned.
#
# A binary names the objects it uses in its debug map (`OSO` entries, which
# `nm -ap` prints), so the sweep is exact: an object no binary names is
# dead, and backtraces of every current binary keep their line numbers.
#
# Usage: scripts/sweep-objects.sh [target-dir] [profile]   (default: target debug)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-${CARGO_TARGET_DIR:-$ROOT/target}}"
PROFILE="${2:-debug}"
DIR="$TARGET/$PROFILE"
[ -d "$DIR/deps" ] || { echo "sweep-objects: no $DIR/deps"; exit 0; }
[ "$(uname)" = Darwin ] || { echo "sweep-objects: only macOS keeps objects for debug info"; exit 0; }

DIR="$DIR" python3 - <<'EOF'
import os, sys
# Real paths on both sides: a binary names its objects by absolute,
# normalized path, and the dir may arrive as `…/worktrees/../target-worktrees`.
d = os.path.realpath(os.environ["DIR"])
deps = os.path.join(d, "deps")
objects = set()
binaries = []
for base in (deps, d, os.path.join(d, "examples")):
    if not os.path.isdir(base):
        continue
    for name in os.listdir(base):
        path = os.path.join(base, name)
        if name.endswith(".o"):
            if base == deps:
                objects.add(path)
            continue
        # Executables have no extension; proc macros are dylibs.
        if ("." in name and not name.endswith(".dylib")) or not os.path.isfile(path):
            continue
        if not name.endswith(".dylib") and not os.access(path, os.X_OK):
            continue
        binaries.append(path)
import mmap, struct

N_OSO = 0x66  # the stab naming an object file the debug info lives in
LC_SYMTAB = 0x2


def named_objects(binary):
    """The N_OSO paths in a thin 64-bit Mach-O's symbol table, read
    directly (nm prints every symbol, which took minutes over a suite)."""
    try:
        with open(binary, "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
            if m[:4] != b"\xcf\xfa\xed\xfe":  # MH_MAGIC_64, little-endian
                return set()
            ncmds = struct.unpack_from("<I", m, 16)[0]
            off = 32
            for _ in range(ncmds):
                cmd, size = struct.unpack_from("<II", m, off)
                if cmd == LC_SYMTAB:
                    symoff, nsyms, stroff, _ = struct.unpack_from("<IIII", m, off + 8)
                    names = set()
                    for i in range(nsyms):
                        strx, ntype = struct.unpack_from("<IB", m, symoff + 16 * i)
                        if ntype == N_OSO:
                            start = stroff + strx
                            names.add(m[start:m.find(b"\0", start)].decode())
                    return names
                off += size
    except (OSError, ValueError, struct.error):
        pass
    return set()

used = set()
from concurrent.futures import ThreadPoolExecutor
with ThreadPoolExecutor(max_workers=os.cpu_count() or 8) as pool:
    for names in pool.map(named_objects, binaries):
        used |= {os.path.realpath(n) for n in names if n.endswith(".o")}
dead = objects - used
# Guard: binaries that name no object at all here while objects exist means
# the paths did not line up; delete nothing rather than everything.
if objects and binaries and not (objects & used):
    print(f"sweep-objects: none of the {len(objects)} objects in {deps} is named by "
          f"its {len(binaries)} binaries; refusing to delete (paths do not match?)",
          file=sys.stderr)
    sys.exit(1)
# rustc's own scratch (`rustcXXXXXX`), left when a build is killed; an hour
# old is long past any build that could still be using it.
import shutil, time
stale_scratch = [
    os.path.join(deps, n) for n in os.listdir(deps)
    if n.startswith("rustc") and os.path.isdir(os.path.join(deps, n))
    and time.time() - os.path.getmtime(os.path.join(deps, n)) > 3600
]
for path in stale_scratch:
    shutil.rmtree(path, ignore_errors=True)
freed = 0
for path in dead:
    try:
        freed += os.path.getsize(path)
        os.remove(path)
    except FileNotFoundError:
        pass
print(f"sweep-objects: {len(binaries)} binaries use {len(objects & used)} objects; "
      f"removed {len(dead)} dead ({freed / 1e9:.1f} GB)"
      + (f" and {len(stale_scratch)} stale rustc scratch dirs" if stale_scratch else ""))
left = len(os.listdir(deps))
if left > 60000:
    print(f"sweep-objects: {deps} still holds {left} files (stale rmeta/rlib "
          "from changed features or Cargo.lock); `cargo clean --profile dev` clears them",
          file=sys.stderr)
EOF
