#!/usr/bin/env python3
"""The referee.

Drives any dedup implementation as a subprocess, drives the oracle beside it on
the same trees, and decides whether the candidate is telling the truth. Nothing
here trusts the candidate: every number it reports is held against either the
oracle's instrumented count or a floor derived from how the tree was built, and
every deletion is judged by what is left on disk rather than by what the tool
said it did.

    python3 conformance/run.py --candidate '<command template>' --gate m2

The template is a shell-ish command line with four substitutions: {root} for
the directory to scan, {json} for where the protocol report goes, {flags} for
the scan flags the case calls for, and {stage} — empty except under the M1
gate, where it expands to the flag that stops the oracle after size grouping so
a half-built implementation can be judged on the half it has.

Exit 0 when the gate is green, 1 when the candidate is wrong, 2 when the
machine or the harness is.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import trees                                          # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
ORACLE = os.path.join(HERE, "oracle_driver.py")
DEDUP = os.path.join(REPO, "dedup.py")

SEED = 20260822
TIMEOUT = 900

# How far a candidate's self-reported reading may stray from what the oracle
# actually read. REWRITE.md §5 sets it; it is a band, not a ceiling, because
# reading far less than the oracle means skipping bytes the proof needed.
BYTE_TOLERANCE = 0.10

# A warm scan is allowed a rounding error's worth of reading, no more.
WARM_CEILING = 0.05


class Fatal(Exception):
    """Something about the machine or the harness is wrong — exit 2."""


class Case:
    """One tree scanned one way."""

    def __init__(self, name: str, tree: str, flags=()):
        self.name = name
        self.tree = tree
        self.flags = list(flags)


CASES = [
    Case("mixed", "mixed"),
    Case("mixed-minsize", "mixed", ["--min-size", "262144"]),
    Case("mixed-ext", "mixed", ["--ext", "jpg"]),
    Case("mixed-skipext", "mixed", ["--skip-ext", "tmp,txt"]),
    Case("mixed-exclude", "mixed", ["-x", "backup", "-x", "*.tmp"]),
    Case("smallzoo", "smallzoo"),
    Case("smallzoo-j1", "smallzoo", ["-j", "1"]),
    Case("prefix", "prefix"),
    Case("bighead", "bighead"),
    Case("bighead-j8", "bighead", ["-j", "8"]),
    Case("links", "links"),
    Case("adversarial", "adversarial"),
    Case("deletion", "deletion"),
]

# The trees the cache and deletion gates rebuild for themselves, because both
# of them change what they scan.
M3_TREE = "mixed"
M4_TREES = ("deletion", "links")


# ── running things ───────────────────────────────────────────────────────────


def run(cmd, env=None, timeout=TIMEOUT):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, env=env,
                              timeout=timeout)
    except subprocess.TimeoutExpired:
        raise Fatal("timed out after {}s: {}".format(timeout, " ".join(cmd[:4])))
    except OSError as e:
        raise Fatal("cannot run {}: {}".format(cmd[0], e))


def scan_env(cache_dir=None, home=None) -> dict:
    """A scan's environment: never the real cache, never the real Trash."""
    env = dict(os.environ)
    env["DEDUP_CACHE_DIR"] = cache_dir or os.path.join(STATE, "no-cache")
    if home:
        env["HOME"] = home
    return env


def expand(template: str, root: str, json_path: str, flags, stage: str) -> list:
    """Turn the candidate template into an argument list.

    Substitution happens token by token after the template is split, so a path
    with a space in it stays one argument and a flag list expands into several.
    """
    out = []
    for token in shlex.split(template):
        if token == "{flags}":
            out.extend(flags)
        elif token == "{stage}":
            out.extend(shlex.split(stage))
        else:
            out.append(token.replace("{root}", root).replace("{json}", json_path))
    return out


def read_report(path: str, proc, who: str) -> dict:
    """The protocol object, or a Fatal explaining why there isn't one."""
    try:
        with open(path) as fh:
            obj = json.load(fh)
    except (OSError, ValueError) as e:
        raise Fatal("{} wrote no readable report ({}). exit={} stderr={}".format(
            who, e, proc.returncode, (proc.stderr or "")[-2000:]))
    if not isinstance(obj, dict):
        raise Fatal("{}: report is not a JSON object".format(who))
    if obj.get("ok") is False:
        raise Fatal("{} failed: {}".format(who, obj.get("error", "?")))
    missing = [k for k in ("groups", "bytes_read", "files_seen", "bytes_seen",
                           "unreadable", "opens") if k not in obj]
    if missing:
        raise Fatal("{}: report is missing {}".format(who, ", ".join(missing)))
    if not isinstance(obj["groups"], list) or any(
            not isinstance(g, list) or any(not isinstance(p, str) for p in g)
            for g in obj["groups"]):
        raise Fatal("{}: 'groups' must be a list of lists of strings".format(who))
    return obj


def scan(template, root, out_json, flags, stage="", env=None, who="candidate"):
    if os.path.exists(out_json):
        os.unlink(out_json)
    cmd = expand(template, root, out_json, flags, stage)
    proc = run(cmd, env=env)
    return read_report(out_json, proc, who), proc


ORACLE_TEMPLATE = ("{} {} {{root}} --report-json {{json}} {{flags}} {{stage}}"
                   .format(shlex.quote(sys.executable), shlex.quote(ORACLE)))


# ── the answer key, and how a report is judged against one ───────────────────


def normalize_ext(piece: str) -> str:
    return piece.strip().lstrip(".").lower()


def parse_flags(flags) -> dict:
    """The scan flags that change which files are in scope."""
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("--min-size", type=int, default=1)
    p.add_argument("-x", "--exclude", action="append", default=[])
    p.add_argument("--ext", action="append", default=[])
    p.add_argument("--skip-ext", action="append", default=[])
    known, _ = p.parse_known_args(list(flags))

    def collect(raw):
        return frozenset(normalize_ext(piece) for arg in raw
                         for piece in arg.split(",") if normalize_ext(piece))

    include = collect(known.ext) if known.ext else None
    return {"min_size": known.min_size, "include_ext": include,
            "exclude_ext": collect(known.skip_ext), "excludes": known.exclude}


def answer_key(manifest: dict, flags) -> dict:
    return trees.expected(manifest, **parse_flags(flags))


def show(paths) -> str:
    """A group, short enough to read and long enough to identify."""
    if len(paths) > 4:
        return "[{}, ... {} more]".format(", ".join(repr(p) for p in paths[:4]),
                                          len(paths) - 4)
    return "[{}]".format(", ".join(repr(p) for p in paths))


def group_difference(want, got) -> str:
    """The first place two group listings part company, in words.

    Order is part of the answer — REWRITE.md §3.5 makes it so — hence the
    index-by-index walk rather than a set comparison. When the difference is
    not really an ordering one the set view underneath says so.
    """
    for i, (w, g) in enumerate(zip(want, got)):
        if list(w) != list(g):
            wset, gset = {tuple(x) for x in want}, {tuple(x) for x in got}
            if tuple(w) in gset and tuple(g) in wset:
                return ("group {} is out of order — expected {} but got {}, "
                        "and both appear elsewhere in the listing. Groups sort "
                        "by (-size, basename of keeper, keeper path)."
                        .format(i, show(w), show(g)))
            return "group {} differs — expected {} but got {}".format(
                i, show(w), show(g))
    if len(want) > len(got):
        return "{} groups are missing, the first being {}".format(
            len(want) - len(got), show(want[len(got)]))
    return "{} groups too many, the first being {}".format(
        len(got) - len(want), show(got[len(want)]))


def check_sets(key: dict, got: dict, stage_only: bool = False) -> str:
    """Groups and walk counters. Everything both M1 and M2 care about.

    Under M1 the candidate has no content stages yet, so what it calls a group
    is a size bucket, and the answer key it is held to is the size buckets the
    tree's construction record implies.
    """
    want_groups = [list(g) for g in
                   (key["size_groups"] if stage_only else key["groups"])]
    got_groups = [list(g) for g in got["groups"]]
    if want_groups != got_groups:
        return "wrong groups — " + group_difference(want_groups, got_groups)
    for field in ("files_seen", "bytes_seen"):
        if got[field] != key[field]:
            return "wrong {} — expected {} but got {}".format(
                field, key[field], got[field])
    if not stage_only and sorted(got["unreadable"]) != key["unreadable"]:
        return ("wrong unreadable list — expected {} but got {}"
                .format(key["unreadable"], sorted(got["unreadable"])))
    return ""


def check_effort(key: dict, oracle: dict, got: dict) -> str:
    """What the candidate says it read, held against two independent bounds."""
    floor = key["byte_floor"]
    if got["bytes_read"] < floor:
        return ("bytes_read {} is below the floor of {} — proving a group means "
                "reading every byte of every member of it this scan, and the "
                "tree says that is at least {} bytes"
                .format(got["bytes_read"], floor, floor))
    ref = oracle["bytes_read"]
    if ref:
        gap = (got["bytes_read"] - ref) / ref
        if abs(gap) > BYTE_TOLERANCE:
            return ("bytes_read {} is {:+.1%} against the oracle's instrumented "
                    "{} — the band is ±{:.0%}"
                    .format(got["bytes_read"], gap, ref, BYTE_TOLERANCE))
    if got["opens"] > oracle["opens"]:
        return "opens {} exceeds the oracle's {}".format(
            got["opens"], oracle["opens"])
    return ""


def check_determinism(first: dict, second: dict) -> str:
    if [list(g) for g in first["groups"]] != [list(g) for g in second["groups"]]:
        return ("two runs over the same tree reported different groups — " +
                group_difference([list(g) for g in first["groups"]],
                                 [list(g) for g in second["groups"]]))
    for field in ("files_seen", "bytes_seen", "opens", "bytes_read"):
        if first[field] != second[field]:
            return ("two runs over the same tree reported different {} ({} then "
                    "{})".format(field, first[field], second[field]))
    if sorted(first["unreadable"]) != sorted(second["unreadable"]):
        return "two runs over the same tree reported different unreadable lists"
    return ""


# ── gates ────────────────────────────────────────────────────────────────────


def tree_root(base: str, name: str) -> str:
    return os.path.join(trees.tree_dir(base, name, SEED), "tree")


def gate_walk(template: str, base: str, stage: str) -> list:
    """M1 and M2 share a shape: scan every case twice and judge the reports."""
    results = []
    for case in CASES:
        manifest = trees.ensure(base, case.tree, SEED)
        root = tree_root(base, case.tree)
        flags = ["--min-size", "1"] if not any(
            f == "--min-size" for f in case.flags) else []
        flags = flags + case.flags + ["--no-cache"]
        key = answer_key(manifest, flags)
        env = scan_env()

        oracle, _ = scan(ORACLE_TEMPLATE, root, report_path("oracle", case.name),
                         flags, stage, env, "oracle")
        # The oracle is the referee's own witness, so it is checked against the
        # tree's construction record before it is used to judge anyone.
        problem = check_sets(key, oracle, stage_only=bool(stage))
        if problem:
            raise Fatal("the oracle disagrees with the {} tree's manifest on "
                        "case {}: {}".format(case.tree, case.name, problem))

        got, _ = scan(template, root, report_path("cand", case.name), flags,
                      stage, env)
        problem = check_sets(key, got, stage_only=bool(stage))
        if not problem and not stage:
            problem = check_effort(key, oracle, got)
        if not problem:
            again, _ = scan(template, root, report_path("cand2", case.name),
                            flags, stage, env)
            problem = check_determinism(got, again)
        results.append((case.name, problem))
        print("  {:24s} {}".format(case.name, problem or "pass"))
        if problem:
            print("      {}".format(problem))
    return results


def report_path(who: str, name: str) -> str:
    return os.path.join(STATE, "reports", "{}-{}.json".format(who, name))


def next_tick() -> int:
    """Wait for the wall clock to cross a second, and return the new one.

    A cache row stamped in the same second as the file it describes is not
    trustworthy — the file could have changed again inside that second on a
    filesystem that only records whole seconds — so a warm scan is only a fair
    test once the clock has moved on.
    """
    start = int(time.time())
    while int(time.time()) == start:
        time.sleep(0.02)
    return int(time.time())


def gate_cache(template: str) -> list:
    """M3. Cold reads everything, warm reads nothing, and a row the cache has
    no right to trust is not trusted."""
    results = []
    base = os.path.join(STATE, "m3")
    shutil.rmtree(base, ignore_errors=True)
    manifest = trees.build(base, M3_TREE, SEED)
    root = tree_root(base, M3_TREE)
    cache = os.path.join(STATE, "m3-cache")
    shutil.rmtree(cache, ignore_errors=True)
    env = scan_env(cache)
    flags = ["--min-size", "1"]
    key = answer_key(manifest, flags)

    # The tree was written a moment ago. Until the clock crosses a second its
    # files are no older than the rows about to describe them, and every one of
    # those rows would be discarded as racily clean — so the warm scan would
    # measure the racily-clean rule instead of the cache.
    next_tick()
    cold, _ = scan(template, root, report_path("cand", "m3-cold"), flags, "", env)
    problem = check_sets(key, cold, False)
    if not problem and cold["bytes_read"] < key["byte_floor"]:
        problem = ("cold scan read {} bytes with an empty cache, below the "
                   "floor of {}".format(cold["bytes_read"], key["byte_floor"]))
    results.append(("m3-cold", problem))
    print("  {:24s} {}".format("m3-cold", problem or
                               "pass ({:.1f} MB read)".format(cold["bytes_read"] / 1e6)))

    warm, _ = scan(template, root, report_path("cand", "m3-warm"), flags, "", env)
    problem = check_sets(key, warm, False)
    if not problem:
        share = warm["bytes_read"] / cold["bytes_read"] if cold["bytes_read"] else 1.0
        if share > WARM_CEILING:
            problem = ("warm scan of an unchanged tree read {} bytes, {:.1%} of "
                       "the cold scan's {} — nothing was reused"
                       .format(warm["bytes_read"], share, cold["bytes_read"]))
    results.append(("m3-warm", problem))
    print("  {:24s} {}".format("m3-warm", problem or
                               "pass ({:.1f} MB read)".format(warm["bytes_read"] / 1e6)))

    for name, problem in gate_trust(template):
        results.append((name, problem))
        print("  {:24s} {}".format(name, problem or "pass"))
    return results


# The trust checks run on their own small tree. Two of them need a scan to
# begin and end inside one wall-clock second, and a tree that takes half a
# second to scan turns that into a coin toss.
TRUST_TREE = "deletion"
TRUST_VICTIM = os.path.join("copies", "g00-copy1.bin")


def rewrite(path: str, salt: int) -> None:
    """Different bytes, same length. The file stops being anyone's duplicate
    without giving the cache a size change to notice."""
    size = os.path.getsize(path)
    with open(path, "wb") as fh:
        fh.write(bytes((i * salt + 11) % 251 for i in range(size)))


def gate_trust(template: str) -> list:
    """The two halves of §3.3: when the cache may be believed, and when not.

    A row is trustworthy only if the file's mtime second is strictly older than
    the second the row was written. Both checks below build a row that matches
    the file on every column — inode, size, mtime to the nanosecond — and
    differ only in whether that one comparison holds.
    """
    base = os.path.join(STATE, "m3-trust")
    cache = os.path.join(STATE, "m3-trust-cache")
    env = scan_env(cache)
    flags = ["--min-size", "1"]

    def fresh():
        trees.unlock(base)
        shutil.rmtree(base, ignore_errors=True)
        shutil.rmtree(cache, ignore_errors=True)
        manifest = trees.build(base, TRUST_TREE, SEED)
        return manifest, tree_root(base, TRUST_TREE)

    # ── the row is older than the file: the cache is entitled to believe it ──
    manifest, root = fresh()
    victim = os.path.join(root, TRUST_VICTIM)
    original_ns = os.stat(victim).st_mtime_ns
    next_tick()
    scan(template, root, report_path("cand", "m3-trust-cold"), flags, "", env)
    warm, _ = scan(template, root, report_path("cand", "m3-trust-warm"), flags,
                   "", env)
    stale_key = answer_key(manifest, flags)
    trusted = warm["bytes_read"] < stale_key["byte_floor"]
    control = "" if trusted else (
        "skipped — a warm scan re-read {} bytes, so no row was ever trusted "
        "and the rule about trusting the wrong one cannot be exercised"
        .format(warm["bytes_read"]))

    rewrite(victim, 53)
    os.utime(victim, ns=(original_ns, original_ns))
    poison = poison_refusal(template, root, flags, cache, stale_key,
                            TRUST_VICTIM) if trusted else control
    results = [("m3-poison-refusal", poison)]

    # ── the row is as new as the file: it may not be believed ───────────────
    problem = control
    if trusted:
        problem = "skipped — no scan fitted inside a single second"
        for _attempt in range(8):
            manifest, root = fresh()
            victim = os.path.join(root, TRUST_VICTIM)
            second = next_tick()
            os.utime(victim, ns=(second * 1_000_000_000,) * 2)
            before = int(time.time())
            scan(template, root, report_path("cand", "m3-racy-cold"), flags,
                 "", env)
            if before != second or int(time.time()) != second:
                continue        # the row was not stamped in the victim's second
            rewrite(victim, 37)
            os.utime(victim, ns=(second * 1_000_000_000,) * 2)
            racy, _ = scan(template, root, report_path("cand", "m3-racy"),
                           flags, "", env)
            problem = check_sets(answer_key(edited(manifest, TRUST_VICTIM), flags),
                                 racy, False)
            if problem:
                problem = ("a file changed inside the second its cache row was "
                           "written was trusted anyway: " + problem)
            break
    results.append(("m3-same-second", problem))
    return results


def edited(manifest: dict, rel: str) -> dict:
    """The manifest with one file's content declared unique.

    Editing a file in place makes it a duplicate of nothing. Rather than
    re-deriving the tree, the record for that name gets a content hash nothing
    else can share, which is what the edit did to the tree itself.
    """
    entries = [dict(e, sha="edited-in-place-" + rel) if rel in e["names"] else e
               for e in manifest["entries"]]
    return dict(manifest, entries=entries)


def group_holding(key: dict, rel: str) -> set:
    """Every name in the group the given file belongs to."""
    for group in key["detail"]:
        for member in group["members"]:
            if rel in member["names"]:
                return {n for m in group["members"] for n in m["names"]}
    raise Fatal("{} is in no group; the poison case has nothing to poison"
                .format(rel))


def poison_refusal(template, root, flags, cache, key, victim_rel) -> str:
    """Delete against a poisoned cache and see what survives.

    The group the cache vouched for is no longer a group. Reporting it is
    allowed — the cache narrows, it never proves — but removing anything from
    it is not, and the only witness that cannot be talked round is the tree
    afterwards.
    """
    home = os.path.join(STATE, "m3-home")
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(os.path.join(home, ".Trash"))
    env = scan_env(cache, home)
    protected = group_holding(key, victim_rel)
    before = snapshot(root)
    cmd = expand(template, root, report_path("cand", "m3-poison-delete"),
                 list(flags) + ["--auto", "shortest", "-y"], "")
    run(cmd, env=env)
    after = snapshot(root)

    for name in sorted(protected - after):
        return ("{!r} was deleted. The cache said its group was proven; the "
                "bytes said otherwise, and a group that fails to re-prove has "
                "to be refused whole".format(name))
    # A refusal that refuses everything is not a pass either: the rest of the
    # tree was never poisoned and should have been cleaned up as usual.
    if not before - after:
        return ("nothing at all was deleted — refusing the poisoned group is "
                "right, refusing every other group along with it is not")
    return ""


def snapshot(root: str) -> set:
    out = set()
    for dirpath, _, filenames in os.walk(root):
        for fn in filenames:
            out.add(os.path.relpath(os.path.join(dirpath, fn), root))
    return out


def doomed_names(key: dict) -> set:
    """Who lives and who dies, worked out from the tree's construction record.

    The oracle is not asked. The keeper is the shortest path in the group, ties
    broken lexicographically, which is what --auto shortest means; a member the
    scan cannot free — because a name outside the tree still points at it — is
    never a victim however duplicate it is.
    """
    doomed = set()
    for group in key["detail"]:
        members = sorted(group["members"], key=lambda m: (len(m["name"]), m["name"]))
        for member in members[1:]:
            if member["deletable"]:
                doomed.update(member["names"])
    return doomed


def gate_deletion(template: str) -> list:
    """M4. Two trees built the same way, one handed to each implementation."""
    results = []
    for name in M4_TREES:
        problem = ""
        for who, runner in (("oracle", None), ("candidate", template)):
            base = os.path.join(STATE, "m4-{}".format(who))
            shutil.rmtree(base, ignore_errors=True)
            manifest = trees.build(base, name, SEED)
            root = tree_root(base, name)
            home = os.path.join(STATE, "m4-home-{}".format(who))
            shutil.rmtree(home, ignore_errors=True)
            os.makedirs(os.path.join(home, ".Trash"))
            env = scan_env(os.path.join(STATE, "m4-cache-{}".format(who)), home)
            flags = ["--min-size", "1", "--no-cache", "--auto", "shortest", "-y"]
            key = answer_key(manifest, flags)
            before = snapshot(root)

            if runner is None:
                cmd = [sys.executable, DEDUP, root] + flags + ["-q"]
            else:
                cmd = expand(runner, root,
                             report_path("cand", "m4-{}".format(name)), flags, "")
            proc = run(cmd, env=env)
            after = snapshot(root)
            trashed = set(os.listdir(os.path.join(home, ".Trash")))

            doomed = doomed_names(key)
            survived_wrongly = sorted(doomed & after)
            killed_wrongly = sorted((before - after) - doomed)
            if killed_wrongly:
                problem = ("{} deleted {} file(s) it had no business deleting, "
                           "the first being {!r}".format(
                               who, len(killed_wrongly), killed_wrongly[0]))
            elif survived_wrongly:
                problem = ("{} left {} duplicate(s) behind, the first being "
                           "{!r}".format(who, len(survived_wrongly),
                                         survived_wrongly[0]))
            elif trashed != {os.path.basename(p) for p in doomed}:
                problem = ("{}: the Trash holds {} name(s), the tree lost {} — "
                           "every removed name goes to the Trash of its own "
                           "volume".format(who, len(trashed), len(doomed)))
            if problem:
                problem += " (exit {})".format(proc.returncode)
                break
        results.append(("m4-{}".format(name), problem))
        print("  {:24s} {}".format("m4-" + name, problem or "pass"))
        if problem:
            print("      {}".format(problem))
    problem = gate_trashes(template)
    results.append(("m4-trashes", problem))
    print("  {:24s} {}".format("m4-trashes", problem or "pass"))
    return results


def gate_trashes(template: str) -> str:
    """`.Trashes/<uid>` on a volume that is not the boot volume.

    The rule only shows itself on a second volume, so the harness makes one: a
    disk image on the local disk, mounted for the length of this check. Where
    that is not allowed the check says so rather than passing quietly.
    """
    image = os.path.join(STATE, "conformance-volume.dmg")
    mount = None
    try:
        if os.path.exists(image):
            os.unlink(image)
        made = subprocess.run(
            ["hdiutil", "create", "-size", "80m", "-fs", "APFS", "-volname",
             "dedupconf", "-quiet", image],
            capture_output=True, text=True, timeout=180)
        if made.returncode != 0:
            return "skipped — no second volume available ({})".format(
                (made.stderr or "").strip()[:120])
        attached = subprocess.run(
            ["hdiutil", "attach", image, "-nobrowse", "-quiet"],
            capture_output=True, text=True, timeout=180)
        if attached.returncode != 0:
            return "skipped — could not attach the test volume ({})".format(
                (attached.stderr or "").strip()[:120])
        mount = "/Volumes/dedupconf"
        if not os.path.isdir(mount):
            return "skipped — the test volume did not appear at {}".format(mount)

        base = os.path.join(mount, "conformance")
        manifest = trees.build(base, "deletion", SEED)
        root = tree_root(base, "deletion")
        home = os.path.join(STATE, "m4-home-volume")
        shutil.rmtree(home, ignore_errors=True)
        os.makedirs(os.path.join(home, ".Trash"))
        env = scan_env(os.path.join(STATE, "m4-cache-volume"), home)
        flags = ["--min-size", "1", "--no-cache", "--auto", "shortest", "-y"]
        key = answer_key(manifest, flags)
        doomed = doomed_names(key)

        cmd = expand(template, root, report_path("cand", "m4-volume"), flags, "")
        run(cmd, env=env)

        volume_trash = os.path.join(mount, ".Trashes", str(os.getuid()))
        home_trash = os.listdir(os.path.join(home, ".Trash"))
        if home_trash:
            return ("fail: {} name(s) went to the home Trash from a file on "
                    "another volume, the first being {!r} — a move across "
                    "volumes is a copy, which is why the rule exists"
                    .format(len(home_trash), home_trash[0]))
        if not os.path.isdir(volume_trash):
            return ("fail: nothing was written to {} — deletions on a "
                    "non-boot volume belong in that volume's .Trashes"
                    .format(volume_trash))
        got = set(os.listdir(volume_trash))
        want = {os.path.basename(p) for p in doomed}
        if got != want:
            return ("fail: {} holds {} name(s), {} were removed".format(
                volume_trash, len(got), len(want)))
        return ""
    except (subprocess.TimeoutExpired, OSError) as e:
        return "skipped — {}".format(e)
    finally:
        if mount and os.path.isdir(mount):
            subprocess.run(["hdiutil", "detach", mount, "-quiet", "-force"],
                           capture_output=True, timeout=120)
        if os.path.exists(image):
            try:
                os.unlink(image)
            except OSError:
                pass


# ── selftest ─────────────────────────────────────────────────────────────────


SABOTAGES = [
    ("drop-last-group", "m2",
     "drops the last group out of its report"),
    ("halve-bytes", "m2",
     "halves the bytes_read it admits to"),
    ("overdelete", "m4",
     "deletes one file more than it should"),
]


def selftest(base: str) -> int:
    """Prove the gates have teeth.

    A harness that has never failed anything is not a harness. Each doctored
    candidate below is wrong in exactly one way, and the run is only a pass if
    each one is caught by the gate that is supposed to catch it — and if the
    undoctored oracle sails through the same gate.
    """
    passed = True
    print("selftest — three doctored candidates, one honest one\n")

    honest = ORACLE_TEMPLATE
    for gate in ("m1", "m2", "m3", "m4"):
        try:
            failures = [n for n, p in gate_of(gate)(honest, base) if p and
                        not p.startswith("skipped")]
            ok = not failures
        except Fatal as e:
            ok, failures = False, [str(e)]
        passed &= ok
        print("\n  control {:22s} {}".format(
            gate, "clean" if ok else "UNEXPECTED FAILURE: {}".format(failures[:1])))

    for name, gate, what in SABOTAGES:
        script = os.path.join(HERE, "selftest", name.replace("-", "_") + ".py")
        if not os.path.exists(script):
            raise Fatal("missing sabotage script: {}".format(script))
        template = "{} {} {{root}} --report-json {{json}} {{flags}} {{stage}}".format(
            shlex.quote(sys.executable), shlex.quote(script))
        try:
            results = gate_of(gate)(template, base)
            caught = [(n, p) for n, p in results if p and not p.startswith("skipped")]
        except Fatal as e:
            caught = [("harness", str(e))]
        passed &= bool(caught)
        print("\n  {:30s} {}".format(name, "caught" if caught else "MISSED"))
        print("      it {}, and {}".format(
            what,
            "{} failed: {}".format(caught[0][0], caught[0][1]) if caught
            else "the {} gate let it through".format(gate)))

    print("\nselftest {}".format("passed" if passed else "FAILED"))
    return 0 if passed else 1


def gate_of(gate: str):
    if gate == "m1":
        return lambda t, b: gate_walk(t, b, "--stage size")
    if gate == "m2":
        return lambda t, b: gate_walk(t, b, "")
    if gate == "m3":
        return lambda t, _b: gate_cache(t)
    if gate == "m4":
        return lambda t, _b: gate_deletion(t)
    raise Fatal("unknown gate: {}".format(gate))


# ── entry point ──────────────────────────────────────────────────────────────


STATE = ""


def main(argv=None) -> int:
    global STATE
    ap = argparse.ArgumentParser(
        prog="conformance/run.py",
        description="judge a dedup implementation against the oracle")
    ap.add_argument("--candidate", metavar="TEMPLATE",
                    help="command template with {root} {json} {flags} {stage}")
    ap.add_argument("--gate", default="all",
                    choices=("m1", "m2", "m3", "m4", "all"))
    ap.add_argument("--base", default=os.path.join(HERE, ".trees"),
                    metavar="DIR", help="where the trees live (built on demand)")
    ap.add_argument("--selftest", action="store_true",
                    help="prove the gates catch what they claim to")
    args = ap.parse_args(argv)

    base = os.path.abspath(args.base)
    STATE = os.path.join(base, ".state")
    os.makedirs(os.path.join(STATE, "reports"), exist_ok=True)

    if not os.path.exists(DEDUP):
        print("conformance: no oracle at {}".format(DEDUP), file=sys.stderr)
        return 2

    try:
        if args.selftest:
            return selftest(base)
        if not args.candidate:
            print("conformance: --candidate is required", file=sys.stderr)
            return 2

        gates = ["m1", "m2", "m3", "m4"] if args.gate == "all" else [args.gate]
        results = []
        for gate in gates:
            print("\n{} gate".format(gate.upper()))
            results.extend((gate, n, p) for n, p in gate_of(gate)(args.candidate, base))

        failures = [(g, n, p) for g, n, p in results
                    if p and not p.startswith("skipped")]
        skipped = [(g, n, p) for g, n, p in results if p.startswith("skipped")]
        print("\n{} check(s), {} failed{}".format(
            len(results), len(failures),
            ", {} skipped".format(len(skipped)) if skipped else ""))
        for _g, n, p in skipped:
            print("  {}: {}".format(n, p))
        if failures:
            print("\nGATE FAILURE")
            for _g, n, p in failures:
                print("  {}: {}".format(n, p))
            return 1
        print("gate {}: green".format(args.gate))
        return 0
    except Fatal as e:
        print("conformance: {}".format(e), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
