#!/usr/bin/env bash
# Regenerates THIRD-PARTY-NOTICES.md: the bundled assets (hand-written below) and every crate linked into the Linux
# binaries (from cargo's dependency graph, normal deps only: build tools and other platforms aren't shipped), plus
# the NOTICE files Apache-2.0 crates ship. Run after changing dependencies.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo tree --locked -e normal --target x86_64-unknown-linux-gnu --prefix none \
    --format '{p}|{l}|{r}' >"${TMPDIR:-/tmp}/dirigent-tree.$$"
cargo metadata --locked --format-version 1 >"${TMPDIR:-/tmp}/dirigent-meta.$$"

python3 -I - "${TMPDIR:-/tmp}/dirigent-tree.$$" "${TMPDIR:-/tmp}/dirigent-meta.$$" >THIRD-PARTY-NOTICES.md <<'PY'
import json, os, re, sys

tree, meta = sys.argv[1], sys.argv[2]
metadata = json.load(open(meta))
ours = {p["name"] for p in metadata["packages"] if p["id"] in metadata["workspace_members"]}
crates = {}
for line in open(tree):
    p, lic, repo = (line.rstrip("\n").replace(" (*)", "").split("|") + ["", ""])[:3]
    m = re.match(r"(\S+) v(\S+)", p)
    if not m or m.group(1) in ours or "(proc-macro)" in p:
        continue
    crates[(m.group(1), m.group(2))] = (lic or "see repository", repo)

# NOTICE files shipped by the linked crates (Apache-2.0 §4d asks to pass them on).
notices = []
for pkg in metadata["packages"]:
    if (pkg["name"], pkg["version"]) not in crates:
        continue
    d = os.path.dirname(pkg["manifest_path"])
    for f in sorted(os.listdir(d)):
        if f.upper().startswith("NOTICE"):
            notices.append((pkg["name"], pkg["version"], open(os.path.join(d, f), errors="replace").read().strip()))

print("""# Third-party notices

Dirigent itself is MIT-licensed (see `LICENSE`). It bundles or links the components below, each under its own license.
Nothing in the shipped binary is GPL/AGPL. One crate is MPL-2.0 (`option-ext`, file-level copyleft, used unmodified via
`dirs`; its source is on crates.io). Crates offered under a choice of licenses (e.g. `self_cell`: Apache-2.0 OR
GPL-2.0) are used under the permissive option.

Regenerate with `scripts/third-party-notices.sh`.

## Bundled assets

| Component | License | Text |
|---|---|---|
| Inter 4.1 (font) | SIL Open Font License 1.1 | `crates/app/assets/fonts/Inter-LICENSE.txt` |
| JetBrains Mono 2.304 (font) | SIL Open Font License 1.1 | `crates/app/assets/fonts/JetBrainsMono-OFL.txt` |
| Lucide 1.68 (icons) | ISC | `crates/app/assets/icons/LICENSE.txt` |
| tree-sitter grammars and their highlight queries | MIT | `crates/syntax/licenses/` (one file per grammar, list in its README) |

## Frameworks

- **gpui** and the other Zed crates it needs (`gpui_*`, `collections`, `refineable`, `sum_tree`, `scheduler`,
  `http_client`, …) from https://github.com/zed-industries/zed: Apache-2.0. Only Apache-2.0 Zed crates are used;
  none of Zed's GPL-3.0 crates are, and no code was copied from them.
- **alacritty_terminal** (terminal emulation): Apache-2.0.

## Linked crates
""")
print(f"{len(crates)} crates linked into the Linux build (`cargo tree -e normal --target x86_64-unknown-linux-gnu`).\n")
print("| Crate | Version | License | Source |")
print("|---|---|---|---|")
for (name, ver), (lic, repo) in sorted(crates.items(), key=lambda k: (k[0][0].lower(), k[0][1])):
    print(f"| {name} | {ver} | {lic} | {repo} |")
if notices:
    print("\n## NOTICE files\n")
    for name, ver, text in notices:
        print(f"### {name} {ver}\n\n```\n{text}\n```\n")
PY
rm -f "${TMPDIR:-/tmp}/dirigent-tree.$$" "${TMPDIR:-/tmp}/dirigent-meta.$$"
echo "THIRD-PARTY-NOTICES.md: $(grep -c '^| ' THIRD-PARTY-NOTICES.md) table rows"
