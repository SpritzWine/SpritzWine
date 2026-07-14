# Why SpritzWine exists

The rest of the documentation explains *how* this project works: the branch model, the pin
chain, the traps in the build. None of it says what the project is **for**. This does.

The one-line version, which is also the Codeberg tagline:

> **Sanely adding value to WINE leveraging Rust.**

Every word in that sentence is load-bearing, and the one doing the most work is *sanely*.

---

## Think big, act small

The ambition is real: Wine is a thirty-year-old C codebase reimplementing the Windows API, and
large parts of it are exactly the kind of code that Rust is good at — parsing untrusted bytes,
walking descriptors, unwinding stacks. There is a lot of value on the table.

The method is deliberately unambitious. **Start where it brings real value in safety,
performance or correctness — and move on when there's little left.** One subsystem at a time,
each one shipped and proven on real games, rather than accumulating in a branch nobody runs.

That second half is the part people skip. A subsystem is not converted until it is *pure*; it is
converted until the remaining C in it has stopped being a problem. Then you leave, and go find
the next place where the bugs actually live. Grinding a module from 90% to 100% Rust for the
aesthetics of it is how a project like this dies with nothing shipped.

## Rust is a means, not the goal

**This is not "rewrite Wine in Rust."** Nobody needs that, and nobody would finish it.

Rust is a tool applied where C is genuinely hurting: memory-unsafe parsers, descriptor and
protocol handling, unwinders — the bug-prone corners, where a malformed input from a device or a
game is one bounds check away from ruining your afternoon.

Look at what actually ships in Rust today — not what is written, but what is in the Wine you get:

| DLL | What is Rust |
| --- | --- |
| `hidparse.sys` | the HID **report-descriptor parser** |
| `hid` | the `HidP_*` **value engine** (extracting fields out of report buffers) |
| `avrt` | MMCSS — thread scheduling classes |
| `comdlg32` | the file chooser, via the XDG desktop portal |

Two of those four are *literally* "parse a byte blob a USB device handed you and index into it,"
and a third is a protocol client. That is not a coincidence, and it is not where you land if
your goal is "Rust everywhere" — it is where you land if your goal is "fewer bugs." The
conversion targets pick themselves; you go where the memory unsafety is dense enough to be worth
the FFI seam.

(`grep -l lwine_ dlls/*/Makefile.in` in the wine tree answers this question for whatever you
have checked out. Trust that over this table.)

## Prove it where it hurts

The base we ship on is **Valve's patched Wine** — the tree they maintain for Proton, and the
reason anti-tamper titles work at all. (It is Wine, not Proton: Proton is the whole compatibility
tool — Wine plus DXVK, vkd3d, the Steam bridge, the launch script. Our answer to *that* is Prater.
What we take from Valve is the Wine underneath it.)

It is a **stable** release of that Wine, not a bleeding-edge one, and nothing from newer upstream
Wine is smuggled in alongside it: the shipped tree is Valve's, plus our tiers, and nothing else.

The tiers themselves, though, are authored against a **newer upstream Wine** than the one they
land on, and replayed onto Valve's older base — so where the two disagree, the **base wins**. That
is not a footnote; it is where the real work happens. Valve's tree still links tomcrypt and
libunwind where upstream had already moved to symcrypt and dropped libunwind; its
`virtual_unwind()` takes an argument ours does not. Each of those is a decision made during the
replay, in favour of the base. Two bases, one direction of flow — the current versions and the
exact stitch decisions live in [`BRANCHING.md`](BRANCHING.md), where they
can be kept honest.

Riding a stable base is not conservatism for its own sake — it is the only way the experiment
produces a signal. Oxidation on a base that already works means that when a game breaks, the
answer is *"our Rust broke it."* Oxidise on a moving base and every regression comes with a free
debate about whether Wine moved under you, and you spend the project bisecting somebody else's
tree.

The same reasoning drives the whole `soda-*` tier model: the Rust work is kept as a set of clean,
replayable tiers precisely so that it *can* be lifted onto a different Wine base. That is not a
promise about the future — it is what the current replay already does, and a rehearsal for
whatever base comes next. The base and the changes never grow into each other. The fork is
designed to be peeled off.

And the same soda pours into more than one glass. This is the part that is easiest to get
backwards, so it is worth being blunt about which glass is which.

**`spritzwine`** — the tiers poured over *upstream* WineHQ — **is the endeavour.** It is the
project's actual point: Rust in Wine, tracking what Wine is becoming rather than what Valve
froze. It carries the full mix, and it is where fixing plain software counts as much as fixing
games — the battery and power reporting in `soda-ntdll` exists because Windows programs ask how
much charge is left and Wine had no good answer. No frametime was harmed or improved.

**`spritzvalve`** — the tiers over Valve's Proton Wine — **is the necessity.** The gate this
project actually respects is *does the game boot?*, and games boot on Valve's tree, because that
is the tree with the anti-tamper patchset in it. So spritzvalve is the glass we can hand to
someone, the one Prater is built from, and the one that proves the Rust does not break real
software. It carries the thin mix on purpose.

The honest ordering, then: **spritzvalve is how we prove SpritzWine is safe. SpritzWine is the
thing being proven.** Shipping first is not the same as being the point — and today, only
spritzvalve exists as a generated consumer. The upstream glass is next.

## The one standing bet: new-WoW64

For all the holding-still above, every glass ships one deliberately bold architectural choice:
**there is no 32-bit unix side in this Wine at all.** 32-bit Windows programs run as real 32-bit
PE code — compiled by clang/lld, no mingw — on top of a single 64-bit unix layer. One build, one
unix ntdll, both bitnesses.

Upstream Wine built that mechanism, and it is where Wine is heading; but Valve's shipping Proton
still carries a classic native 32-bit unix side. We flipped the switch everywhere and live with
both halves of that: the upside is real — no 32-bit host libraries needed, ever, on distros that
are busy dropping them — and so is the bill. Running a path that upstream builds but nobody
ships means finding the bugs first — and, as the retraction below records, being the first to
blame it for bugs it never had. That trade — early to the future, first to its bugs — is
accepted, not accidental.

## Correctness is proven by games, not by tests alone

Wine's conformance tests are necessary and not sufficient. A Rust `RtlVirtualUnwind2` that
passes every test in the suite and then fails to unwind through a real game's exception handler
is not a working unwinder; it is a working test fixture.

So the project's real gate — `BRANCHING.md` calls it exactly this, **the boot gate** — is:
*does the game boot?*

Currently: **Avatar: Frontiers of Pandora**, **Risk of Rain 2**, **Watch Dogs: Legion**. Avatar
is the interesting one. It crashed around 13 seconds in on the vanilla-Wine fork (an
anti-tamper interaction with copy-on-write semantics — see the AFOP notes); on the Valve base it
plays, at ~67 fps on the author's machine. One machine, no benchmark suite, no claim of a
speedup over stock Proton — just the difference between a game that runs and a game that
doesn't.

That gate is why `soda-glorious` (DualSense rumble) and `soda-ntdll` (threadpool,
`RtlVirtualUnwind2`/SEH, LZNT1, battery) are **written but not shipping**. The code exists. It
is deliberately held back, unpublished, in tiers that no released consumer includes — because an
untested consumer on a remote is an invitation for somebody to pin it, and then you own it. They
ship when they pass the gate, not when they compile.

There is also **[beisl](https://codeberg.org/SpritzWine/beisl)**, the performance tracer, which
is the same instinct applied to speed: it is deliberately Wine-agnostic and traces stock Proton
just as happily as ours, because a performance claim you can't reproduce against the thing you
claim to beat isn't a claim.

## Honest about what's broken

Being candid about open bugs is part of the tone here — including when the bug turns out to be
ours. This section used to document an ntsync deadlock in 32-bit CEF launchers, and we shipped a
per-game opt-out for it. It was never real: under ntsync *every* wait is an ioctl, so the threads
we found parked in `/dev/ntsync` ioctls were a healthy process, not a hung one. Both Ubisoft
titles boot with ntsync on, and the opt-out is gone.

A bug you only ever confirm is not a bug you have investigated.

## Upstreaming: welcome, not a conquest

If a piece of this pays off, the right home for it is upstream — Wine or Proton — and the
project would be glad to push it there. The fork getting *smaller* is a good outcome, not a
defeat. This is explicitly **not** an embrace-extend-extinguish play; SpritzWine has no interest
in becoming the Wine you have to use and somebody else needs to maintain.

Read that as an attitude, not a roadmap. There is no upstreaming milestone, no date, no patch
series in flight. Wine's Rust story is Wine's to decide. What this project can do in the
meantime is the unglamorous part: demonstrate, on a stable base, with real games, that a
particular subsystem is better off in Rust — and have working code sitting there when somebody
asks. Everything is LGPL-2.1-or-later; cherry-pick it freely, no permission needed.

---

## The names

They are all one joke, and it is a Viennese one. It also happens to describe the architecture
accurately, which is the only reason it survived.

**SpritzWine** — from the *G'spritzter* (Spritzer): white wine cut with sparkling water, the
unofficial house drink of Vienna. Wine, mixed with something that makes it lighter. The full
toast, and the Häupl quote it comes from, is in the [README](../README.md#the-name--man-bringe-den-spritzwein).

**`soda-*`** — the soda is what you mix *into* the wine. And this is not decoration: it is
precisely how the branch model works. The `soda-*` tiers carry no Wine of their own — they are
the syrup, and a shipping branch is *wine base + soda*, poured together by
`tools/regenerate.sh`. The metaphor pays rent.

**Prater** — the Wiener Prater, Vienna's great public park and funfair (the Riesenrad, the
chestnut avenues, the place you actually go to *play*). It is the distribution you install: the
Proton fork, where the games are.

**beisl** — a Viennese *Beisl* is a small, unpretentious neighbourhood pub. It is the
performance tracer: where you sit down afterwards and go over what just happened, at length,
loudly.

The owner's summary: *"they came for the fun and stayed for the performance."* The brand is the
door. The engineering is the room.
