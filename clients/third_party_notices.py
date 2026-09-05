#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Generate THIRD_PARTY_NOTICES.md (and, optionally, a CycloneDX SBOM) from the shipping
dependency graphs of this tree -- then a person reviews it (the notices rule:
"generate, then review; do not rely on an unreviewed scanner dump").

    py third_party_notices.py                       # writes THIRD_PARTY_NOTICES.md beside this tree's root
    py third_party_notices.py --check               # exit 1 if the committed file's component table is stale
    py third_party_notices.py --sbom out.cdx.json   # also write a CycloneDX 1.5 SBOM of the same components

Ecosystems, each detected by what the tree carries:

  cargo    every Cargo.toml workspace root (`cargo metadata`, normal dependencies only --
           what the shipped binary links -- resolved for the shipping target when one is
           configured; license texts and copyright lines read from the registry source
           `cargo` already downloaded, so the run is offline).
  python   a pyproject.toml project, read from an installed environment (`--python-venv`),
           because only an installed set is the resolved set: `importlib.metadata`
           distributions, their License-Expression / License / classifiers, and the
           license files their metadata names.
  nuget    a .csproj project restored on this machine (`obj/project.assets.json`) -- the
           package ids and versions -- with each package's .nuspec and license file read
           from the NuGet global packages folder.
  gradle   a build.gradle.kts project: `gradlew -q dependencies --configuration
           runtimeClasspath` for the resolved coordinates, then each artifact's POM (and
           its parent chain) from the Gradle module cache for the license.

Only runtime dependencies are listed: what a licensee receives in the shipped artifact.
Build-time and test-only dependencies are not distributed and are governed by their own
licenses at build time; `--include-build` adds cargo's build dependencies for a fuller SBOM.

The notices file lists every component (name, version, license, source), then the
distinct license texts the components carry (deduplicated by their text with copyright
lines removed), and the copyright lines each component's own license file states, so an
attribution requirement (MIT, BSD, ISC, Apache NOTICE) is met by the file as a whole.
Anything the tools cannot determine is written as UNKNOWN, never guessed; a reviewer
resolves it before release.

Stdlib only. This file is the same in every Munarium product tree (server/tools/,
matrix/scripts/, clients/); a change goes to all three.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The tree root: the directory that carries the notices file. The generator lives at the
# root (clients/) or one level down (server/tools/, matrix/scripts/).
ROOT = HERE if (HERE / "THIRD_PARTY_NOTICES.md").exists() or (HERE / "LICENSE").exists() or (HERE / "LICENSE.md").exists() else HERE.parent
OUT = ROOT / "THIRD_PARTY_NOTICES.md"

LICENSE_FILE_RE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)([-._].*)?$", re.I)
COPYRIGHT_RE = re.compile(r"copyright\s*(\(c\)|©|\d{4})", re.I)


@dataclass
class Component:
    ecosystem: str
    name: str
    version: str
    license: str  # SPDX expression, or UNKNOWN
    source: str  # repository / homepage / registry
    purl: str
    texts: dict[str, str] = field(default_factory=dict)  # filename -> content
    copyrights: list[str] = field(default_factory=list)

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.ecosystem, self.name, self.version)


def run(cmd: list[str], cwd: Path | None = None, env: dict | None = None) -> str:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=True, encoding="utf-8", errors="replace", env=env).stdout


def read_license_files(d: Path) -> tuple[dict[str, str], list[str]]:
    texts: dict[str, str] = {}
    lines: list[str] = []
    if not d.is_dir():
        return texts, lines
    for p in sorted(d.iterdir()):
        if p.is_file() and LICENSE_FILE_RE.match(p.name) and p.stat().st_size < 200_000:
            try:
                t = p.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            texts[p.name] = t
            for ln in t.splitlines():
                s = ln.strip().strip("#/*-<!>").strip()
                if COPYRIGHT_RE.search(s) and len(s) < 200 and s not in lines:
                    lines.append(s)
    return texts, lines[:6]


# --- cargo -----------------------------------------------------------------------------

def cargo_components(root: Path, include_build: bool, target: str | None) -> list[Component]:
    cmd = ["cargo", "metadata", "--format-version", "1", "--locked"]
    if target:
        cmd += ["--filter-platform", target]
    meta = json.loads(run(cmd, cwd=root))
    members = set(meta["workspace_members"])
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    kinds = {"normal", "build"} if include_build else {"normal"}
    seen: set[str] = set()
    stack = list(members)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes.get(pid, {}).get("deps", []):
            for k in dep.get("dep_kinds", [{"kind": None}]):
                kind = k.get("kind") or "normal"
                if kind in kinds and dep["pkg"] not in seen:
                    stack.append(dep["pkg"])
    out = []
    for pid in seen - members:
        p = by_id[pid]
        cdir = Path(p["manifest_path"]).parent
        texts, lines = read_license_files(cdir)
        lic = p.get("license") or ("(license file only)" if p.get("license_file") else "UNKNOWN")
        out.append(Component("cargo", p["name"], p["version"], lic, p.get("repository") or p.get("homepage") or "crates.io",
                             f"pkg:cargo/{p['name']}@{p['version']}", texts, lines))
    return out


# --- python ------------------------------------------------------------------------------

PY_DUMP = r'''
import importlib.metadata as m, json, sys
out = []
for d in m.distributions():
    md = d.metadata
    name = md["Name"]; ver = md["Version"]
    lic = md.get("License-Expression") or ""
    if not lic:
        cls = [c for c in md.get_all("Classifier") or [] if c.startswith("License ::")]
        lic = "; ".join(c.split("::")[-1].strip() for c in cls) if cls else (md.get("License") or "")
        if lic and len(lic) > 120: lic = lic.splitlines()[0][:120] + " ..."
    texts = {}
    for f in md.get_all("License-File") or []:
        try:
            t = d.read_text("licenses/" + f) or d.read_text(f)
            if t: texts[f] = t
        except Exception: pass
    urls = md.get_all("Project-URL") or []
    home = md.get("Home-page") or (urls[0].split(",",1)[-1].strip() if urls else "")
    out.append({"name": name, "version": ver, "license": lic or "UNKNOWN", "source": home or "PyPI", "texts": texts})
json.dump(out, sys.stdout)
'''


def python_components(venv: Path, exclude: set[str]) -> list[Component]:
    py = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    data = json.loads(run([str(py), "-c", PY_DUMP]))
    out = []
    for d in data:
        if d["name"].lower().replace("_", "-") in exclude or d["name"].lower() in {"pip", "setuptools", "wheel"}:
            continue
        lines = []
        for t in d["texts"].values():
            for ln in t.splitlines():
                s = ln.strip()
                if COPYRIGHT_RE.search(s) and len(s) < 200 and s not in lines:
                    lines.append(s)
        out.append(Component("pypi", d["name"], d["version"], d["license"], d["source"],
                             f"pkg:pypi/{d['name'].lower()}@{d['version']}", d["texts"], lines[:6]))
    return out


# --- nuget -------------------------------------------------------------------------------

def nuget_components(project_dir: Path) -> list[Component]:
    assets = project_dir / "obj" / "project.assets.json"
    if not assets.is_file():
        raise SystemExit(f"{assets} missing: run `dotnet restore` in {project_dir} first")
    a = json.loads(assets.read_text(encoding="utf-8"))
    pkgs_root = Path(os.environ.get("NUGET_PACKAGES") or (Path.home() / ".nuget" / "packages"))
    out = []
    for key, lib in a.get("libraries", {}).items():
        if lib.get("type") != "package":
            continue
        name, ver = key.split("/", 1)
        pdir = pkgs_root / name.lower() / ver
        lic, src = "UNKNOWN", "nuget.org"
        texts: dict[str, str] = {}
        nuspec = pdir / f"{name.lower()}.nuspec"
        if nuspec.is_file():
            try:
                x = ET.parse(nuspec).getroot()
                ns = {"n": x.tag.split("}")[0].strip("{")} if "}" in x.tag else {}
                md = x.find("n:metadata", ns) if ns else x.find("metadata")
                if md is not None:
                    def g(tag):
                        e = md.find(f"n:{tag}", ns) if ns else md.find(tag)
                        return e
                    le = g("license")
                    if le is not None and le.text:
                        if le.get("type") == "expression":
                            lic = le.text.strip()
                        else:
                            lic = "(license file only)"
                            lf = pdir / le.text.strip()
                            if lf.is_file():
                                texts[lf.name] = lf.read_text(encoding="utf-8", errors="replace")
                    elif g("licenseUrl") is not None and g("licenseUrl").text:
                        lic = "see " + g("licenseUrl").text.strip()
                    pu = g("projectUrl") if g("projectUrl") is not None else g("repository")
                    if pu is not None:
                        src = (pu.text or pu.get("url") or src).strip()
            except ET.ParseError:
                pass
        t2, lines = read_license_files(pdir)
        texts.update(t2)
        lines2 = []
        for t in texts.values():
            for ln in t.splitlines():
                s = ln.strip()
                if COPYRIGHT_RE.search(s) and len(s) < 200 and s not in lines2:
                    lines2.append(s)
        out.append(Component("nuget", name, ver, lic, src, f"pkg:nuget/{name}@{ver}", texts, (lines or lines2)[:6]))
    return out


# --- gradle ------------------------------------------------------------------------------

COORD_RE = re.compile(r"[\\+|\- ]*([A-Za-z0-9_.\-]+):([A-Za-z0-9_.\-]+):([A-Za-z0-9_.\-]+)(?: -> ([A-Za-z0-9_.\-]+))?")


def gradle_components(project_dir: Path) -> list[Component]:
    # Resolve to absolute first: subprocess's cwd= does not rebase a relative
    # executable path, so a relative project_dir from outside this tree would
    # look for the wrapper relative to the wrong directory.
    project_dir = project_dir.resolve()
    wrapper = project_dir / ("gradlew.bat" if os.name == "nt" else "gradlew")
    txt = run([str(wrapper), "-q", "dependencies", "--configuration", "runtimeClasspath"], cwd=project_dir)
    coords: dict[tuple[str, str], str] = {}
    for ln in txt.splitlines():
        m = COORD_RE.match(ln)
        if m and ln.lstrip().startswith(("+---", "\\---", "|")) or (m and ln.startswith(("+", "\\"))):
            g, a, v, resolved = m.groups()
            coords[(g, a)] = resolved or v
    cache = Path.home() / ".gradle" / "caches" / "modules-2" / "files-2.1"
    out = []
    for (g, a), v in sorted(coords.items()):
        lic, src, texts = pom_license(cache, g, a, v, depth=0)
        out.append(Component("maven", f"{g}:{a}", v, lic, src, f"pkg:maven/{g}/{a}@{v}", texts, []))
    return out


def pom_license(cache: Path, g: str, a: str, v: str, depth: int) -> tuple[str, str, dict[str, str]]:
    d = cache / g / a / v
    poms = list(d.glob(f"*/{a}-{v}.pom")) if d.is_dir() else []
    if not poms:
        return "UNKNOWN (POM not in the Gradle cache)", "Maven Central", {}
    try:
        x = ET.parse(poms[0]).getroot()
    except ET.ParseError:
        return "UNKNOWN (unparseable POM)", "Maven Central", {}
    ns = "{" + x.tag.split("}")[0].strip("{") + "}" if "}" in x.tag else ""
    lics = [(l.findtext(f"{ns}name") or "").strip() for l in x.findall(f"{ns}licenses/{ns}license")]
    url = (x.findtext(f"{ns}url") or x.findtext(f"{ns}scm/{ns}url") or "Maven Central").strip()
    if lics:
        return "; ".join(l for l in lics if l), url, {}
    parent = x.find(f"{ns}parent")
    if parent is not None and depth < 6:
        pg = (parent.findtext(f"{ns}groupId") or "").strip(); pa = (parent.findtext(f"{ns}artifactId") or "").strip(); pv = (parent.findtext(f"{ns}version") or "").strip()
        lic, purl, t = pom_license(cache, pg, pa, pv, depth + 1)
        return lic, url if url != "Maven Central" else purl, t
    return "UNKNOWN (no <licenses> in the POM chain)", url, {}


# --- rendering ---------------------------------------------------------------------------

def normalize_text(t: str) -> str:
    return re.sub(r"\s+", " ", "\n".join(ln for ln in t.splitlines() if not COPYRIGHT_RE.search(ln))).strip().lower()


def render(comps: list[Component], product: str, inputs: list[str]) -> str:
    comps = sorted(comps, key=lambda c: (c.ecosystem, c.name.lower(), c.version))
    counts: dict[str, int] = {}
    for c in comps:
        counts[c.license] = counts.get(c.license, 0) + 1
    lines = [f"# Third-party notices — {product}", "",
             f"Generated {dt.date.today().isoformat()} by `{HERE.name}/{Path(__file__).name}` from: " + "; ".join(inputs) + ".",
             "Runtime dependencies only. Each component is governed by its own license, which takes",
             "precedence for that component; the texts and copyright statements below are reproduced",
             "from the components' own license files. Reviewed by: _(name, date)_.", "",
             f"## Summary — {len(comps)} components", "", "| License | Components |", "|---|---|"]
    for lic, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"| {lic} | {n} |")
    for eco in sorted({c.ecosystem for c in comps}):
        lines += ["", f"## {eco}", "", "| Component | Version | License | Source |", "|---|---|---|---|"]
        for c in comps:
            if c.ecosystem == eco:
                lines.append(f"| {c.name} | {c.version} | {c.license} | {c.source} |")
    # distinct license texts
    texts: dict[str, tuple[str, list[str]]] = {}
    for c in comps:
        for fname, t in c.texts.items():
            h = hashlib.sha256(normalize_text(t).encode()).hexdigest()[:12]
            if h not in texts:
                texts[h] = (t.strip(), [])
            texts[h][1].append(f"{c.name} {c.version} ({fname})")
    lines += ["", "## Copyright statements", "",
              "From each component's own license file, where one carries them.", ""]
    for c in comps:
        if c.copyrights:
            lines.append(f"- **{c.name} {c.version}**: " + " · ".join(c.copyrights))
    lines += ["", f"## License texts — {len(texts)} distinct", "",
              "Each text once, followed by the components whose license file is that text (identical",
              "after removing copyright lines and whitespace).", ""]
    for h, (t, users) in sorted(texts.items(), key=lambda kv: -len(kv[1][1])):
        lines += [f"### Text {h} — {len(users)} component(s)", "", "Used by: " + ", ".join(users), "", "```text", t, "```", ""]
    return "\n".join(lines) + "\n"


def sbom(comps: list[Component], product: str) -> dict:
    return {
        "bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
        "metadata": {"timestamp": dt.datetime.now(dt.timezone.utc).isoformat(), "component": {"type": "application", "name": product},
                     "tools": [{"name": Path(__file__).name}]},
        "components": [{"type": "library", "name": c.name, "version": c.version, "purl": c.purl,
                        "licenses": [{"expression": c.license}] if c.license and c.license != "UNKNOWN" and not c.license.startswith(("see ", "(", "UNKNOWN")) else [],
                        "externalReferences": [{"type": "vcs" if "github" in c.source else "website", "url": c.source}] if c.source.startswith("http") else []}
                       for c in sorted(comps, key=lambda c: (c.ecosystem, c.name.lower(), c.version))],
    }


def component_table(text: str, ecosystems: set[str] | None = None) -> set[str]:
    """The component rows, tagged by the `## <ecosystem>` section they sit in; with
    `ecosystems`, only those sections -- so a check that resolved cargo alone compares
    cargo alone and says nothing about rows it did not regenerate."""
    rows: set[str] = set()
    section = ""
    fenced = False
    for ln in text.splitlines():
        if ln.startswith("```"):
            fenced = not fenced  # a license text may carry its own `## ` lines
        elif fenced:
            continue
        elif ln.startswith("## "):
            section = ln[3:].strip()
        elif ln.startswith("| ") and ln.count("|") == 5 and not ln.startswith("| Component") and not ln.startswith("| License"):
            if ecosystems is None or section in ecosystems:
                rows.add(f"{section}: {ln}")
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--product", default=None, help="the product name in the heading (default: from LICENSE.md / NOTICE)")
    ap.add_argument("--cargo-target", default=None, help="resolve cargo dependencies for this target triple (the shipping image's)")
    ap.add_argument("--include-build", action="store_true", help="also list cargo build dependencies")
    ap.add_argument("--python-venv", action="append", default=[], help="an environment with a Python project of this tree installed")
    ap.add_argument("--python-exclude", action="append", default=[], help="distribution names to leave out (the project itself)")
    ap.add_argument("--dotnet-project", action="append", default=[], help="a restored .csproj directory")
    ap.add_argument("--gradle-project", action="append", default=[], help="a Gradle project directory with a wrapper")
    ap.add_argument("--no-cargo", action="store_true")
    ap.add_argument("--sbom", default=None, help="also write a CycloneDX 1.5 JSON SBOM here")
    ap.add_argument("--check", action="store_true", help="compare the component table with the committed file; exit 1 on drift")
    ap.add_argument("--out", default=str(OUT))
    args = ap.parse_args()

    product = args.product
    if not product:
        for cand in ("NOTICE.md", "NOTICE"):
            p = ROOT / cand
            if p.is_file():
                product = p.read_text(encoding="utf-8").splitlines()[0].lstrip("# ").strip()
                if product.lower() == "notices" and len(p.read_text(encoding="utf-8").splitlines()) > 2:
                    product = p.read_text(encoding="utf-8").splitlines()[2].strip()
                break
    product = product or ROOT.name

    comps: list[Component] = []
    inputs: list[str] = []
    if not args.no_cargo:
        for manifest in sorted(ROOT.rglob("Cargo.toml")):
            if any(part in {"target", "node_modules"} for part in manifest.parts):
                continue
            txt = manifest.read_text(encoding="utf-8")
            if "[workspace]" in txt or "[package]" in txt and not any(m.is_file() for m in manifest.parents if False):
                pass
            if "[workspace]" not in txt:
                continue
            wroot = manifest.parent
            comps += cargo_components(wroot, args.include_build, args.cargo_target)
            inputs.append(f"`{wroot.relative_to(ROOT).as_posix() or '.'}/Cargo.lock` via `cargo metadata`" + (f" ({args.cargo_target})" if args.cargo_target else ""))
    for v in args.python_venv:
        comps += python_components(Path(v), {e.lower().replace('_', '-') for e in args.python_exclude})
        inputs.append("the installed Python environment")
    for d in args.dotnet_project:
        comps += nuget_components(Path(d))
        inputs.append(f"`{Path(d).resolve().relative_to(ROOT).as_posix()}/obj/project.assets.json`")
    for d in args.gradle_project:
        comps += gradle_components(Path(d))
        inputs.append(f"`{Path(d).resolve().relative_to(ROOT).as_posix()}` runtimeClasspath")
    # one row per (ecosystem, name, version)
    uniq: dict[tuple[str, str, str], Component] = {}
    for c in comps:
        uniq.setdefault(c.key, c)
    comps = list(uniq.values())

    text = render(comps, product, inputs)
    out = Path(args.out)
    if args.check:
        if not out.is_file():
            print(f"{out}: missing"); return 1
        ecos = {c.ecosystem for c in comps}
        old, new = component_table(out.read_text(encoding="utf-8"), ecos), component_table(text, ecos)
        if old != new:
            for ln in sorted(new - old): print(f"+ {ln}")
            for ln in sorted(old - new): print(f"- {ln}")
            print(f"third_party_notices --check ({', '.join(sorted(ecos))}): {len(new - old)} added, {len(old - new)} removed -- regenerate and review"); return 1
        print(f"third_party_notices --check ({', '.join(sorted(ecos))}): {len(comps)} components, table current -- ok"); return 0
    out.write_text(text, encoding="utf-8", newline="\n")
    unknown = [c for c in comps if c.license.startswith("UNKNOWN")]
    print(f"wrote {out} ({len(comps)} components, {len(unknown)} UNKNOWN license(s))")
    for c in unknown:
        print(f"  UNKNOWN: {c.ecosystem} {c.name} {c.version} -- resolve before release")
    if args.sbom:
        Path(args.sbom).write_text(json.dumps(sbom(comps, product), indent=2), encoding="utf-8", newline="\n")
        print(f"wrote {args.sbom}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
