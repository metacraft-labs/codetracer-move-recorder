#!/usr/bin/env bash
# Regenerates the committed Move compiler debug info for the `flow_test`
# module:
#
#   test-programs/move/flow_test/build/flow_test/debug_info/flow_test.json
#
# The converter reads this sidecar (see `src/move_debug_info.rs`) to map
# bytecode PCs to source lines/columns and slot indices to source-level
# local names. The traces under `flow_test/traces/` were captured from this
# module, so the tests that replay them need its debug info.
#
# Only this one file is kept, and only the fields the loader reads:
# `module_name`, `from_file_path` and, per function, `definition_location`,
# `parameters`, `locals` and `code_map` (byte ranges only), one function per
# line. A full `sui move build` tree is ~1,100 files (bytecode, disassembly,
# and the debug info of every Sui and MoveStdlib dependency) and records
# absolute paths of the machine that built it. `from_file_path` is written package-relative (`sources/flow_test.move`);
# the loader falls back to `<package_root>/sources/<Module>.move` when the
# recorded path does not exist, so the fixture resolves on any checkout.
#
# The module is built on its own, in test mode (`#[test]` functions carry the
# PCs the traces step through), because the other modules in the package are
# Aptos-shaped and do not compile under the Sui toolchain.
#
# Usage (inside the dev shell, which provides `sui` and `python3`):
#
#   test-programs/move/regenerate-flow-test-debug-info.sh          # rewrite
#   test-programs/move/regenerate-flow-test-debug-info.sh --check  # verify
#
# `--check` exits non-zero when the committed file differs from what the
# toolchain produces now. A difference means the compiler lays the module out
# differently from the one the traces were recorded against: re-record the
# traces together with the debug info rather than regenerating one alone.
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)
pkg="$here/flow_test"
out="$pkg/build/flow_test/debug_info/flow_test.json"

mode=write
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

command -v sui >/dev/null || { echo "error: sui not on PATH (enter the dev shell)" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/sources"
cp "$pkg/Move.toml" "$work/"
cp "$pkg/sources/flow_test.move" "$work/sources/"
(cd "$work" && sui move build --test >"$work/build.log" 2>&1) || {
  cat "$work/build.log" >&2
  echo "error: sui move build --test failed" >&2
  exit 1
}

python3 - "$work/build/flow_test/debug_info/flow_test.json" "$work/minimal.json" <<'PY'
import json, sys
src, dst = sys.argv[1], sys.argv[2]
raw = json.load(open(src))

def loc(l):
    # The loader reads a location's byte range only; `file_hash` names the
    # one source file this module has.
    return {"start": l["start"], "end": l["end"]}


def fn_min(fn):
    out = {"definition_location": loc(fn["definition_location"])}
    for f in ("parameters", "locals"):
        out[f] = [[name, loc(l)] for name, l in fn.get(f, [])]
    out["code_map"] = {pc: loc(l) for pc, l in fn.get("code_map", {}).items()}
    return out


fmap = raw["function_map"]
lines = [
    "{",
    ' "version": %s,' % json.dumps(raw["version"]),
    ' "from_file_path": "sources/flow_test.move",',
    ' "module_name": %s,' % json.dumps(raw["module_name"]),
    ' "function_map": {',
]
keys = sorted(fmap, key=int)
for i, k in enumerate(keys):
    sep = "," if i + 1 < len(keys) else ""
    lines.append("  %s: %s%s" % (json.dumps(k), json.dumps(fn_min(fmap[k]), sort_keys=True), sep))
lines += [" }", "}"]
json.loads("\n".join(lines))  # the output must stay valid JSON
with open(dst, "w") as f:
    f.write("\n".join(lines) + "\n")
PY

if [ "$mode" = check ]; then
  if cmp -s "$work/minimal.json" "$out"; then
    echo "ok: $out matches the toolchain's output"
  else
    diff -u "$out" "$work/minimal.json" | head -40 >&2 || true
    echo "error: $out differs from the toolchain's output" >&2
    exit 1
  fi
else
  mkdir -p "$(dirname "$out")"
  cp "$work/minimal.json" "$out"
  echo "wrote $out"
fi
