# REVIEW-M5_1.md — maintainer review of the M5.1 claim

> **CURRENT VERDICT: APPROVED** — 2026-08-23 re-review at `afd67fb`, appended
> at the end of this file. The binding item is fixed and independently
> re-verified against the recipe below, on two separately built binaries whose
> hashes I recorded. The builder also found and fixed a second defect I had only
> noted in passing (exit 101 from ratatui's `Terminal::drop`), and closed the
> hole that let the death test stop covering the browser — both of which I
> confirmed by building the mutants myself. Everything from here to the
> re-review is the original CHANGES review, kept intact as the audit trail.

## Original review (verdict superseded)

**Verdict: CHANGES**

Claimed at `b102625` on branch `features`, reviewed at that tip. Binding specs
are `M5_1-MENU.md`, `M5-TUI.md` (whose laws it says apply unchanged) and
REWRITE.md §0/§3. I read `LOG.md` and `evidence/` first and then verified
independently.

**One binding item**, below. It is not in the deletion path and not in the
one-table machinery — both of those I tried to break and could not. It is
M5-TUI.md hard rule 4, the terminal one, failing on the screen M5.1 added: an
orphaned pre-flight menu burns a whole core, which is the exact failure
`main`'s `09599c6` was written to end and which the browser one screen later
does not have.

Everything else in this milestone is in good shape. The one-table rule is real
and enforced by tests that bite, the menu genuinely cannot express anything the
flags cannot, unshown flags ride through untouched, and the below-54×16 refusal
is correctly applied.

---

## Binding item

### 1. The pre-flight menu spins on a core when its terminal dies

**Severity: high.** This is M5-TUI.md rule 4's territory ("A user's wrecked
terminal is a failed milestone") and it reintroduces, one screen earlier, the
precise bug `09599c6` fixed for the browser — the one the LOG describes as "an
orphaned `dedup-rs -i` was once found burning a whole core for eight hours".

**The defect.** `rust/src/tui/menu.rs:416-423`:

```rust
if crate::sys::wait_for_terminal_input(std::time::Duration::from_millis(250))
    == crate::sys::Input::HungUp
{
    return Ok(None);
}
let Event::Key(key) = event::read()? else {
    continue;
};
```

`wait_for_terminal_input` has three answers and this reads only one of them.
On `Input::Waited` — nothing typed within 250 ms, which is the normal state of
a menu waiting for a keystroke — control falls through to `event::read()`,
which blocks. From that moment the hangup check is unreachable, because the
loop never comes back round. When the terminal then dies, the process is
already inside crossterm's `try_read`, where a descriptor that is readable
forever and empty forever loops on `read` producing no event and no error. That
is exactly what `sys::wait_on`'s own doc comment says cannot be caught at the
call site.

Contrast the browser, `rust/src/tui/mod.rs:167-174`, which consumes all three
and therefore only ever enters `event::read()` when poll has just said bytes
are waiting:

```rust
match sys::wait_for_terminal_input(WAIT_FOR_KEY) {
    sys::Input::Ready => {}
    sys::Input::HungUp => break Ended::TerminalGone,
    sys::Input::Waited => continue,
}
let event = event::read()?;
```

**It is reachable by an ordinary user**, not only by a test harness: with `-i`
(or a bare interactive launch on a terminal) the menu is the screen that is up,
and it sits in that blocking read for as long as the user takes to look at the
form. Close the terminal window in that time and the process is orphaned at
100% of a core.

**Evidence.** The repository's own script, at this tip:

```
$ python3 rust/evidence/scripts/tui-terminal-death.py rust/target/release/dedup-rs
  keys    PASS  'q' quit the browser in 0.64s, exit 0
  hangup  FAIL  still running 5s after the terminal died, 5.05 CPU-seconds in that window — SPINNING
1 scenario(s) failed
```

The same script against a `main@baf9e25` build (no menu, so `-i` reaches the
browser directly) passes:

```
  hangup  PASS  left 0.05s after the terminal died, exit code 1, 0.01 CPU-seconds
all scenarios passed
```

The stuck process, sampled — this is the crossterm loop, named:

```
Call graph:
    854 Thread_28178229   DispatchQueue_1: com.apple.main-thread  (serial)
      ...
              854 dedup_rs::run::hea8409f2e67932f4  (in dedup-rs) + 16612
                854 crossterm::event::read::hbf4301823f308ec3  (in dedup-rs) + 492
                  853 <...UnixInternalEventSource as EventSource>::try_read::h0a6dd3ced235b04c + 464
                  + 850 read  (in libsystem_kernel.dylib) + 8,4
```

and the screen that was up when the terminal died was the menu, not the
browser:

```
=== SCREEN PAINTED ===
dedup-rs — before the scan  root  /var/folders/.../featrev-probe-...
min-size 4096  ext every extension  skip-ext skip nothing  exclude exclude
nothing  jobs as many as the disk is worth  profile auto  no-cache off
up/down move   space edit   enter scan   esc quit
=== END SCREEN ===
```

**Note that `tui-terminal-death.py` catches this only intermittently**, which
is worth knowing before anyone re-runs it and sees a pass. It closes the master
as soon as it sees a gap in the paint, which lands inside the menu's 250 ms
poll about as often as not. Do not take one green run as a fix.

#### Verification recipe

A deterministic driver — the delay before the terminal dies is a parameter, so
the process is provably past its poll and inside `event::read()` when the
terminal goes away. Save as `hangup-deterministic.py`:

```python
#!/usr/bin/env python3
"""usage: hangup-deterministic.py <binary> <delay-seconds> [repeats]"""
import fcntl, os, pty, re, select, shutil, signal, struct, subprocess, sys
import tempfile, termios, time

BIN, DELAY = sys.argv[1], float(sys.argv[2])
REPEATS = int(sys.argv[3]) if len(sys.argv) > 3 else 1
WATCH = 5.0

def corpus(root):
    for name in ("a.bin", "copy-of-a.bin"):
        open(os.path.join(root, name), "wb").write(b"duplicate payload " * 4096)
    open(os.path.join(root, "other.bin"), "wb").write(b"something else " * 4096)

def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)],
                         capture_output=True, text=True).stdout.strip()
    if not out:
        return None
    total = 0.0
    for part in out.split(":"):
        total = total * 60 + float(part)
    return total

def once(tree):
    master, follower = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    pid = os.fork()
    if pid == 0:
        os.setsid()
        os.dup2(follower, 0); os.dup2(follower, 1); os.dup2(follower, 2)
        os.close(master)
        if follower > 2:
            os.close(follower)
        os.execv(BIN, [BIN, tree, "-i", "--min-size", "4096"])
        os._exit(127)
    os.close(follower)
    painted, deadline = b"", time.time() + 10
    while time.time() < deadline:
        r, _, _ = select.select([master], [], [], 0.25)
        if not r:
            if painted:
                break
            continue
        try:
            chunk = os.read(master, 8192)
        except OSError:
            break
        if not chunk:
            break
        painted += chunk
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")
    visible = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "",
                     painted.decode("utf8", "replace"))
    screen = "menu" if "before the scan" in visible else "browser"
    time.sleep(DELAY)                      # provably past the 250 ms poll
    before = cpu_seconds(pid) or 0.0
    os.close(master)                       # the terminal dies here
    end = time.time() + WATCH
    while time.time() < end:
        done, st, usage = os.wait4(pid, os.WNOHANG)
        if done:
            return "LEFT(%s, %.2f CPU-s)" % (screen, usage.ru_utime + usage.ru_stime)
        time.sleep(0.05)
    spent = (cpu_seconds(pid) or 0.0) - before
    os.kill(pid, signal.SIGKILL); os.waitpid(pid, 0)
    return "SPINNING(%s, %.2f CPU-s in %.0fs)" % (screen, spent, WATCH)

tree = tempfile.mkdtemp(prefix="hangup-")
corpus(tree)
spins = 0
for i in range(REPEATS):
    verdict = once(tree)
    spins += verdict.startswith("SPINNING")
    print("  run %d: %s" % (i + 1, verdict))
shutil.rmtree(tree, ignore_errors=True)
print("SPUN %d/%d" % (spins, REPEATS))
sys.exit(1 if spins else 0)
```

At the tip this is 3/3:

```
$ python3 hangup-deterministic.py rust/target/release/dedup-rs 1.5 3
  run 1: SPINNING(menu, 2.07 CPU-s in 5s)
  run 2: SPINNING(menu, 3.20 CPU-s in 5s)
  run 3: SPINNING(menu, 2.55 CPU-s in 5s)
SPUN 3/3
```

**The fix, and proof it is the right one.** Consume all three arms exactly as
the browser does:

```rust
match crate::sys::wait_for_terminal_input(std::time::Duration::from_millis(250)) {
    crate::sys::Input::Ready => {}
    crate::sys::Input::HungUp => return Ok(None),
    crate::sys::Input::Waited => continue,
}
```

I applied that one edit to a scratch copy of the tip, built it, and re-ran
both:

```
$ python3 hangup-deterministic.py <patched build> 1.5 3
  run 1: LEFT(menu, 0.01 CPU-s)
  run 2: LEFT(menu, 0.01 CPU-s)
  run 3: LEFT(menu, 0.01 CPU-s)
SPUN 0/3

$ python3 rust/evidence/scripts/tui-terminal-death.py <patched build>
  keys    PASS  'q' quit the browser in 0.65s, exit 0
  hangup  PASS  left 0.05s after the terminal died, exit code 101, 0.00 CPU-seconds
all scenarios passed
```

To clear this item:

1. Make that change (or an equivalent that cannot enter `event::read()` on
   `Waited`).
2. **Check the exit code on the way out.** My patched build leaves with 101,
   which is a panic exit; `main`'s browser leaves the same scenario with 1.
   It restores the terminal either way — the menu installs a panic hook that
   calls `put_the_terminal_back` — so this is not a wrecked terminal, but a
   panic is not the intended exit and should be run down.
3. Add a regression test. `tui-terminal-death.py` is the natural home, but it
   must be made deterministic first (a fixed delay past the poll, as above) or
   it will pass on a broken build about half the time. Extend it to drive the
   menu explicitly rather than relying on whichever screen happens to be up.
4. Re-run `tui-terminal-death.py` and confirm the `keys` case still passes, so
   the fix does not buy the exit by ignoring input.

---

## Evidence — what I ran myself

Standards at the tip, from a clean rebuild, in `rust/`:

```
$ cargo test
test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.16s

$ cargo fmt --check
FMT_EXIT=0
```

`--gate all` with `{stage}`: **33 check(s), 0 failed / gate all: green**, exit
0. The conformance templates are non-interactive and the menu never opens for
them, as required.

### The one-table rule, and whether the golden test bites

`menu_mutants.py` re-run: **6 mutants, 6 caught**, exit 0, final line `every
mutant was caught`.

I did not take the script's word for it. I reproduced two of them by hand in my
own scratch copy of `rust/` and read the failures out of raw `cargo test`
output:

- **`help-misses-a-flag`** (`for flag in FLAGS {` → `FLAGS.iter().skip(1)` in
  the help renderer, `cli.rs:362` — the only one of three such loops followed
  by `let mut said = wrapped(flag.help, room);`) fails
  `cli::tests::the_help_text_and_the_parser_know_exactly_the_same_flags` and
  `cli::tests::the_help_text_says_something_about_every_flag`, panicking with
  `--min-size has no line`.
- **`menu-drops-a-field`** (`Takes::Nothing if row.on => out.push(...)` →
  `=> {}`, `menu.rs:187`) fails
  `tui::menu::tests::a_switch_toggles_and_reaches_the_options` and
  `tui::menu::tests::the_command_line_prefills_the_fields_it_names`, panicking
  on `assertion failed: options(&form.to_arguments()).no_cache`.

Both bite, in both directions — help↔parser drift and menu→argument-list drift.

### `--help`

`--help` and `-h` both exit **0** with byte-identical output. The parser's
accepted flags and the documented flags match exactly, in both directions —
16 flags, 24 spellings, no flag accepted but undocumented and none documented
but unaccepted.

One honest caveat on "ONE table": there are strictly two. `HELP_FLAGS`
(`cli.rs:262`) is matched by hand at `cli.rs:282` **before** the `FLAGS`
lookup. It is not a drift hole — `help_text()` renders it from that same
constant (`cli.rs:373-377`) and the golden test chains it into the set it diffs
(`cli.rs:407-417`) — but it is a second table, and the milestone doc's "ONE
table (a single source of flag truth)" would read more truthfully with a
sentence acknowledging it. Non-binding.

### The menu cannot exceed the flags, and unshown flags survive it

The design is right: `Form::to_arguments` returns a command line and `main`
re-reads it with `parse_arguments_from`, the same call the CLI uses. A
capability the flags lack would have to be a flag the parser accepts, so the
property holds by construction rather than by discipline.

I drove the real binary on a pty to confirm the ride-through rather than trust
that:

```
=== C. flags the form never shows must ride through Enter ===
   report written to the path given on the command line: yes
   groups: [['a.bin', 'copy-of-a.bin']]
   did the excluded *.tmp leak into a group? False
   ok — both --report-json and -x rode through untouched
```

`-x '*.tmp'` was honoured after a trip through the form (the duplicate
`sub/skipme.tmp` stayed out of the group) and `--report-json` still wrote to
the path given on the command line. The documented divergence — the form's list
fields split on commas, so a glob containing a comma cannot be typed into `-x`
— is the form doing *less* than the flag, which the rule permits, and the
command line still takes such a glob.

### Below 54×16

Correct, and REVIEW-M5's second finding is properly applied one screen earlier.
On a 50×14 pty:

```
=== A. below the 54x16 floor: 50x14, press Enter ===
   screen said: 'the pre-flight menu needs 54x16'
   refusal line shown: True
   did Enter start a scan? False
   ok — refused
```

At exactly 54×16 the form is drawn and usable. And the way out works at every
size — this is the part that matters most, since a refusal you cannot escape is
worse than the thing it refuses:

```
  below the floor / q                50x14          LEFT exit=0 after 0.00s
  below the floor / esc              50x14          LEFT exit=0 after 0.00s
  at the floor / q                   54x16          LEFT exit=0 after 0.00s
  at the floor / esc                 54x16          LEFT exit=0 after 0.00s
  roomy / q                          120x40         LEFT exit=0 after 0.00s
  roomy / esc                        120x40         LEFT exit=0 after 0.00s
STUCK CASES: 0
```

### End-to-end pty script

`m5_1_menu_end_to_end.py` re-run against the release binary — all 7 cases pass,
exit 0:

```
--help exits 0 and lists the flags       ok (exit 0, 47 lines, -h identical)
a piped run never sees the menu          ok (2 groups printed, no menu in the output)
enter-through equals the piped run       ok (2 groups, identical to the piped run)
enter-through with no flags at all       ok (('huge-a.bin', 'huge-b.bin'), the 10 MiB default honoured on both sides)
typing a min-size changes the groups     ok (1 group; the 4 KiB pair was filtered out by the typed value)
space toggles the cache switch           ok (cold 4206592 bytes, warm 12288, switch on 4206592 — the toggle reached the options)
esc scans nothing                        ok (exit 0, no report, nothing read)

every case behaves
```

The cache case asserting on the reported byte count rather than on a phrase in
the pty stream is the right call, and the LOG's account of why (ratatui
repaints only changed cells, so only the word `on` crosses the wire on a later
frame) is correct.

---

## Non-binding notes

1. **"ONE table" is two.** `HELP_FLAGS` alongside `FLAGS`, as above. The
   invariant is enforced; the wording just over-claims.

2. **Piped and flagged non-interactive use is byte-for-byte unchanged.** I
   confirmed this against `main` rather than assuming it: flagless stdout is
   byte-identical (`cmp -s` clean), stderr empty on both, and every counter in
   the report JSON matches at `-j1` and `-j8`. Recorded here because it is an
   M5.1 requirement as much as an M6 one.

3. **Not this branch's:** the attached short-option forms `-j1`,
   `--min-size=1` and `-x*.tmp` are rejected while the oracle accepts `-j1`. I
   checked `main@baf9e25` and it rejects them identically, so the parser
   rewrite onto `FLAGS` did not lose anything — the gap predates this branch.

---

## What I checked and found correct

- `--help`/`-h` exit 0 with every documented flag and its default; the two
  outputs are identical.
- The parser↔help bijection holds in both directions, and the test that
  enforces it fails when either side stops reading the table (hand-verified).
- `Flag::apply` is the single function turning a word into a setting, so the
  menu's field validation really does produce the parser's own error message
  rather than a copy of it.
- The menu builds a command line, not an options struct, and unshown flags are
  kept verbatim and put back.
- Defaults shown in the menu come from the table (`menu-writes-its-own-default`
  is caught).
- Esc/q exits 0 without scanning, at every terminal size.
- The menu never opens for `--list`, `--auto`, or a pipe; `--gate all` is
  unaffected at 33/33.
- `--profile` is in the flag table, in `--help`, and in the menu, and reaches
  the browser's `Compare` as well as `--auto`.

---

## How to clear this review

Fix binding item 1 with the four steps in its recipe, re-run
`tui-terminal-death.py` (made deterministic) and `cargo test` / clippy / fmt /
`--gate all`, and re-claim M5.1. The rest of the milestone needs nothing.

---

# Re-review — 2026-08-23, at `afd67fb`

**Verdict: APPROVED.** Binding item 1 is fixed. I re-verified it against my own
recipe rather than the builder's report, on two independently built binaries,
and I rebuilt both of the new mutants myself to check the tests that guard it
actually bite on the screen they name.

Re-reviewed at `afd67fb` (`17dd456` the fix, `afd67fb` the re-claim), two
commits atop my review commit `533667b`. Tree clean. `ls /Volumes` before the
gate run showed only `dedupbench` — no passport.

## The binaries I tested

I got burned once on this branch by a shared `CARGO_TARGET_DIR` handing me four
identical binaries, so per my own warning:

```
worktree build      d7002edcf24a6b5cd53dacf93ff060b4eeb3f614f3887728dd2cca3fe35c0d02
independent build   502ef37961d56a2e868fc890cc707de18e73634e6e790b182b3c1d0557bd5515
```

The second is `git archive afd67fb` extracted to scratch and built with its own
target directory. The hashes differ only because the source path is embedded;
both were driven through every scenario below and behaved identically, which is
what rules staleness out.

## Item 1 — the menu spun on a dead terminal: **fixed**

The fix is the one the recipe asked for and then some. `menu.rs` now consumes
all three answers, with `Waited => continue`; the `terminal.draw` failure path
checks `hung_up()` first, exactly as the browser does; `hung_up()` is promoted
to `pub(super)` and shared rather than copied; and the hardcoded 250 ms is
replaced by the browser's own `WAIT_FOR_KEY`, so the two screens cannot drift
apart on the timeout either. `ask` returns an `Answer` enum instead of an
`Option`, and `main` maps `Answer::TerminalGone` to exit 1 — the same ending
the browser gives the same event.

The repository's death script, both builds:

```
$ python3 rust/evidence/scripts/tui-terminal-death.py <worktree build d7002ed>
  keys/menu        PASS  'q' left the menu in 0.63s, exit 0
  keys/browser     PASS  'q' left the browser in 0.63s, exit 0
  hangup/menu      PASS  the menu left 0.06s after the terminal died, exit code 1, 0.00 CPU-seconds
  hangup/browser   PASS  the browser left 0.05s after the terminal died, exit code 1, 0.00 CPU-seconds
all scenarios passed
EXIT=0

$ python3 rust/evidence/scripts/tui-terminal-death.py <independent build 502ef37>
  ...four PASS, identical wording, EXIT=0
```

My deterministic driver — the one that matters, because the repo script catches
this only about half the time — at 1.5 s past the poll, three ways:

```
$ python3 rust/evidence/scripts/hangup_deterministic.py <worktree build> 1.5 3
  run 1: LEFT(menu, exit=1, 0.01 CPU-s)
  run 2: LEFT(menu, exit=1, 0.01 CPU-s)
  run 3: LEFT(menu, exit=1, 0.01 CPU-s)
SPUN 0/3

$ python3 <my original scratch driver> <worktree build> 1.5 3      SPUN 0/3
$ python3 <my original scratch driver> <independent build> 1.5 3   SPUN 0/3
```

I diffed the in-repo `hangup_deterministic.py` against the copy I wrote into
the original review: it keeps the delay-past-the-poll, keeps the CPU sampling
and the `SPINNING` threshold, and adds the exit code to the verdict line. It is
my recipe, not a weakened version of it. Keys still work on both screens, so the
fix is not buying the exit by ignoring input.

## The second defect — exit 101 — is real, and the fix is right

I had flagged the 101 in my own patched build as "run this down" without
diagnosing it. The builder diagnosed it: ratatui's `Terminal::drop` tries to
show the cursor and `eprintln!`s when it cannot, which panics when the
descriptor it would print to is the terminal that just died. The fix is
`std::mem::forget(terminal)` on the `TerminalGone` path only — the browser's
existing pattern — with the `Restore` guard still running, so a terminal that
is really there is still restored.

I verified the in-between claim rather than taking it. Starting from `afd67fb`
I removed **only** the `mem::forget`, keeping the three-arm loop and the
`hung_up()` draw check:

```
### three arms, NO mem::forget — deterministic driver ###
  run 1: LEFT(menu, exit=101, 0.01 CPU-s)
  run 2: LEFT(menu, exit=101, 0.01 CPU-s)
  run 3: LEFT(menu, exit=101, 0.01 CPU-s)
SPUN 0/3
```

Exactly as claimed: the poll fix alone stops the spin and leaves the panic
exit; the `mem::forget` is what turns 101 into 1. Both halves were needed and
both are present.

## The per-screen pinning is real — I built both mutants

This was the claim most worth distrusting, since a test that says which screen
it is on is only useful if a menu-only defect leaves the browser case green. I
did not rely on `menu_mutants.py` reporting itself. I built both mutants from
`afd67fb` in my own scratch trees and ran the death script against each:

**`one-of-three-answers`** (revert the loop to the two-way `if`, `mem::forget`
kept) — the original defect:

```
  keys/menu        PASS  'q' left the menu in 0.62s, exit 0
  keys/browser     PASS  'q' left the browser in 0.62s, exit 0
  hangup/menu      FAIL  the menu was still running 5s after the terminal died, 5.03 CPU-seconds in that window — SPINNING
  hangup/browser   PASS  the browser left 0.06s after the terminal died, exit code 1, 0.01 CPU-seconds
1 scenario(s) failed
```

**`drop-the-dead-terminal`** (remove the `mem::forget`, loop kept) — the second
defect:

```
  hangup/menu      FAIL  the menu left in 0.05s but by panicking (exit 101)
  hangup/browser   PASS  the browser left 0.06s after the terminal died, exit code 1, 0.00 CPU-seconds
1 scenario(s) failed
```

In both cases exactly one scenario fails and it names the menu, while the
browser scenario stays green. That is the pinning working, and it is also the
proof that the old single-scenario script could not have caught either — the
regression I found was invisible to it precisely because `-i` stopped reaching
the browser and nothing asserted which screen had been reached. `reach()` now
fails loudly if the expected screen is not the one that painted, which closes
that hole rather than papering over it.

The full battery is 8/8:

```
$ python3 rust/evidence/scripts/menu_mutants.py
  ... six table mutants, all caught ...
Baseline: the same tree, built and run through tui-terminal-death.py.
  every scenario passed
mutant one-of-three-answers         enters the blocking read when poll said nothing was typed
  caught: hangup/menu      FAIL  ... SPINNING
mutant drop-the-dead-terminal       lets ratatui print to a descriptor that is gone, panicking on the way out
  caught: hangup/menu      FAIL  the menu left in 0.05s but by panicking (exit 101)
every mutant was caught
EXIT=0
```

## Standards and the gate, re-run at `afd67fb`

```
$ cargo test
test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s

$ cargo fmt --check
FMT_EXIT=0

$ python3 conformance/run.py --gate all \
    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
33 check(s), 0 failed
gate all: green
[exited with code 0]
```

Non-interactive use is still untouched by the change to `main`'s menu branch:
flagless stdout is byte-identical to the `main@baf9e25` build (`cmp -s` clean),
stderr empty on both. And since `main.rs` was edited, I re-ran the
cohort-boundary subset of my own M6 differential at this tip —
`pair-straddles-boundary`, `pairs-never-meet-the-pivot`, `multi-round-wide` —
all still identical across oracle, flagless, flash, spinning and auto, 0
failures.

## The non-binding notes

All three were taken, and taken openly rather than quietly.

1. **Seek-win sentence:** added, and it says the right thing — the ratios
   belong to "wide groups of large files", small-file trees seek slightly more,
   and "Read '3.0x-3.6x fewer' as a statement about large wide groups, not
   about every tree." It cites its own differential rows rather than asserting
   it.
2. **Stale prose numbers:** corrected, and the correction is *recorded* rather
   than silently patched — the LOG says "Three stale prose numbers corrected in
   the entries above: 158 and 159 tests to 163, and 'six mutants' to seven for
   M6's script." That is the right way to fix a number in a journal.
3. **Two tables:** the one-table section now says plainly that there are
   strictly two, `HELP_FLAGS` beside `FLAGS`, and why that is not a drift hole.

The builder correctly left alone the items I marked "not this branch's" (the
attached `-j1` form, the stale `VolumeProbe` doc comment).

## Ruling

**APPROVED.** Binding item 1 is cleared. The fix is the right shape — it makes
the two screens share the loop, the timeout constant and the hangup question
rather than keeping two copies that drifted once already — and the tests that
guard it fail on the screen they name, which I proved by building both mutants
rather than reading about them.

`REVIEW-M6.md`'s merge banner can come down as far as M5.1 is concerned. One
thing from that file is still outstanding and is not mine to close:
`rbench --selftest` was never run, because it rebuilds a corpus on
`/Volumes/dedupbench` which is outside my fence and was in use by other agents.
Run it as part of merge verification.
