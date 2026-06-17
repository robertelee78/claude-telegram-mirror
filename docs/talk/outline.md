# The Most Fragile Line of Code
### Controlling an AI Agent From Your Phone — 10-minute talk + live demo

**Source project:** `claude-telegram-mirror` (`ctm`) — a Rust bridge between the Claude Code CLI and Telegram.
**Speaker:** Robert E. Lee · github.com/robertelee78/claude-telegram-mirror · linkedin.com/in/robertgpt
**Format:** 10 min, ends in a live phone→terminal demo. Mixed-room default with adapt notes.

> **Status: DRAFT for review (outline only — no slides built yet).**
> Decisions locked with the speaker are recorded in the "Decisions log" at the bottom.

---

## The spine: a 3-act reversal (this is the true, shipped story)

The original outline (and the published CFP abstract) told a 2-act "keystrokes → elegant protocol fix" story. **That fix was reverted.** The shipped code (ADR-015, superseding ADR-014 PR-E) went *back* to keystroke injection — on purpose. The talk now tells the real arc:

- **Act I — the fragile original.** Driving an interactive terminal question from a phone by *simulating a human*: timed keystrokes + a blind `sleep`. Slow, and it races.
- **Act II — the "elegant" rewrite (false summit).** Stop typing; hand the answer back as structured data through a blocking hook. Microseconds. Delete the fragile file. Feels like a clean win.
- **Act III — the reversal.** That fix silently broke the product: handing back structured data tells the CLI "already answered," so **the question stopped rendering in the terminal** — killing ctm's whole reason to exist (answer from *either* surface). The real fix wasn't a better layer; it was making the "fragile" line *verified* instead of blind.

**Why this is the better talk:** the most fragile line gets *deliberately restored* after a clever detour. The lesson resists the glib "simulating a human is a smell" moral and replaces it with something truer.

**Tone arc:** earnest + story-driven on the open (the fire) → punchy, wry, laugh beats through the middle (the hunt-and-peck robot; "I optimized away half my own product") → sincere again on the landing. Voice: first-person "I"; the AI co-builder is downplayed, mentioned only where unavoidable. Jargon allowed with a 4–5-word gloss the first time (e.g., "a hook — a script Claude runs at lifecycle moments"). Reversal is framed as *"the codebase taught me,"* not a heavy mea culpa.

---

## Beat map

| # | Time | Beat | Act |
|---|------|------|-----|
| 1 | 0:00–0:45 | The fire (real story) | I |
| 2 | 0:45–1:45 | What ctm is + plant the *both-surfaces* invariant | I |
| 3 | 1:45–2:45 | The asymmetry + the villain widget | I |
| 4 | 2:45–3:45 | The fragile v1 — the real keystroke code, the race | I |
| 5 | 3:45–4:45 | The "elegant" rewrite — speak the protocol, µs, delete it | II (false summit) |
| 6 | 4:45–5:30 | The rug-pull — it broke the both-surfaces invariant | III |
| 7 | 5:30–6:30 | The real fix — restore injection, make it *verified* | III |
| 8 | 6:30–9:30 | **LIVE demo** — phone quizzes me, both surfaces | — |
| 9 | 9:30–10:00 | Close — takeaways + the Channels line + repo/QR | — |

Budget reality: a ~3-min live demo squeezes the body to ~5.5 min. Target a **8:45 dry-run** so stage nerves don't push past 10:00. Cut from the *body*, never the demo.

---

## Beat 1 — The fire (0:00–0:45) · earnest, no jokes yet

**Point:** open on the *moment* — the real new-baby story. No name, no agenda, no "can everyone hear me." This is the emotional hook; let it be sincere before the comedy ramps.

**Draft script (your real story — trim to ~45s out loud; dial the family detail up/down to taste):**
> "In February of last year, my fourth kid was born. So I'm on paternity leave — which, if you've done it, you know is not *leave*. I'm in my home office, an AI coding agent running on my machine, mid-task — the kind of task you do **not** walk away from and leave running unsupervised. And from the other room, my wife needs a hand with the baby. Now. So I'm stuck: if I leave that terminal, I've got an agent running wild on my codebase; if I don't, I'm leaving my wife holding the baby. All I needed was to *see* what it was doing — and tell it 'yes,' or 'no, not like that' — from the other room. From my phone. As if I were still sitting there."

**The promise (sets up the finale):**
> "That's the whole problem in one sentence: drive my agent from my phone, exactly as if I were at the keyboard. In ten minutes, I'll do that live, in front of you. But first — the one line of code that made it possible was, for months, the most fragile line in the entire project."

**Name, one line, at ~0:45:** "I'm Robert." (Keep it to a beat; the story already earned their attention.)

**Visual:** title slide, or black. No bullet points during the story.

---

## Origin / motivation — color bank (source for Beats 1–3 + Q&A)

The full "why I built the stupid thing" story, in the speaker's own telling. Deploy the pieces where they fit; don't tell all of it (time). Strongest bits are starred.

- ★ **The fire (Beat 1):** Feb 2025, 4th child born; paternity leave; torn between an unsupervised agent in the office and a newborn (and wife) in the next room.
- ★ **Why supervision, not just approvals:** running agents in "YOLO mode" (no gating) is risky — you need to *watch* it, *interrupt* it when it's on the wrong path, and answer it at **any** point — not only when it happens to ask permission. *(This is the design north-star that makes the AskUserQuestion fight matter — questions, not just approvals.)*
- **What I tried first (and why it fell short):** SSH'd in from my phone to drive it — but the screen was tiny and scrolling back through output was miserable. Others had partial setups over SMS / Telegram / Slack — but all of them stopped at *notifications* or *approvals*. I wanted **full context, as if I were sitting there**, and to interact at any moment.
- **The multi-machine reality:** I program from several machines, each running multiple Claude Code sessions at once — so I needed to always know *which computer* and *which session* an update was coming from. (Maps to ctm's per-session Telegram forum topics — optional Beat-2 detail.)
- **The CTM aim (one sentence):** everything on the screen mirrored to Telegram; anything I type in Telegram shows up in the session as if I'd typed it in my home office.
- ★ **The dictation bonus:** phone dictation works cleanly inside Telegram, so I can *talk* to Claude — genuinely useful when you're holding a baby and have one arm free.
- ★ **The payoff that made it real (great demo lead-in or closing callback):** when school started, I could pick my daughter up, let her play at the playground, and not miss a beat on any running job — answering my agents one-handed from a bench.

*(Suggested deployment: open with the fire (Beat 1); use "YOLO → supervise, not just approve" to set up Beat 3's stakes; save the playground/one-arm image as a callback right before the live demo or in the close. The SSH-too-small + others-fell-short bits are ideal Q&A or a single Beat-2 aside.)*

---

## Beat 2 — What ctm is + the invariant (0:45–1:45)

**Point:** just enough to make the demo legible — and **plant the invariant Act III pays off.**

**Beats:**
- One plain sentence: "`ctm` is a Rust daemon — a background process — that bridges the Claude Code CLI and a Telegram thread, in both directions."
- One flow diagram (below). Point at it; you'll point again during the demo.
- **The invariant (load-bearing — say it deliberately, slowly; Act III pays this off):** "The whole point isn't two *copies* of the conversation. It's **one** conversation I can touch from either side. I start at my desk. I get up for coffee, glance at my phone, answer something there. I sit back down and keep typing in the terminal — same session, mid-thought. I never have to *decide* 'okay, now I'm in phone mode.' **The interface follows me. I never switch contexts — I just keep going.**"
  - *(This is the real thesis of the whole project, in the speaker's words: seamless use from the computer OR the phone — either — without ever having to manually switch interface context. Land it clearly here so the audience feels the loss in Beat 6.)*
- Scale: understated — "It's a real, tested codebase — about 20,000 lines of Rust." (Don't recite the full stats; they live on the slide for the handout.)

**Diagram (matches `docs/ARCHITECTURE.md`):**
```
Claude Code hook ─▶ ctm hook ─▶ Unix socket ─▶ daemon ─▶ Telegram
        ▲                                                   │
        └───────────────  tmux send-keys  ◀─────────────────┘
```

**Visual:** the diagram; scale numbers small in the corner (handout).

---

## Beat 3 — The asymmetry + the villain widget (1:45–2:45) · humor starts

**Point:** *out* is easy; *in* is brutal — and the worst "in" is a question.

**Beats:**
- "Going *out* is easy: the agent fires events — a hook — and I forward them. Fire and forget."
- "Coming back *in*? Brutal. Claude Code has no 'inject the answer' API — it reads a **terminal**. That's the only door." *(Accurate: the terminal is the only public input surface. The hook-return trick in Beat 5 is the surprise — don't hint at it here.)*
- Narrow to the single villain: "A yes/no approval, fine — the hook can return that. But sometimes the agent asks a **multiple-choice question** — and it draws it as an interactive terminal widget — a TUI, arrow keys and all. You can't pipe text at that. You have to *drive* it."

**Visual (asset to capture):** a real screenshot of Claude Code's AskUserQuestion multiple-choice widget in the terminal. The room should *see* the arrow-key menu and instantly get why it's hard.

---

## Beat 4 — The fragile v1 (2:45–3:45) · wry, this is the title beat

**Point:** v1 impersonated a human keystroke-by-keystroke — and the linchpin was a blind `sleep` with a comment that did not age well.

**Beats (wry):**
- "So my first version did the only thing it could: it *pretended to be me*. Type the option's number. Press Down, Down, Down to walk the cursor to 'Submit.' Press Enter. One key at a time — 300 milliseconds apart — because if you go faster the terminal drops them."
- "I built a robot… to hunt-and-peck… for another robot."
- The real code (slide):
```rust
let key_delay = Duration::from_millis(300);

for &idx in selected_indices {            // toggle each chosen option
    inj.send_key(&format!("{}", idx + 1));
    sleep(key_delay).await;
}
let downs_needed = total_options + 2;     // walk down to "Submit"
for _ in 0..downs_needed {
    inj.send_key("Down");
    sleep(key_delay).await;
}
inj.send_key("Enter");
```
- The math: "Nine-ish keypresses, 300ms each — call it three seconds. Then I had to wait for the review screen to render before pressing Enter. How long does that take? I had no idea. So I guessed."
- **The most fragile line** (its own slide — this is the title shot):
```rust
sleep(Duration::from_millis(2000)).await;   // 2s is enough
```
- The headline failure (pick ONE, vivid): "Two seconds was *usually* enough. When it wasn't, I pressed Enter into a screen that hadn't rendered yet — and the answer just… evaporated. ~4.7 seconds per answer, riding on a guess." *(Don't say "coin-flip" — the failure was intermittent, not 50/50; ~4.7s is the documented representative figure, ADR-014:217.)*

**Visual:** the keystroke loop, then a full-screen `// 2s is enough`. *(The slide snippet is lightly condensed from the real `dec6e30^` lines — `let _ = inj.send_key(&digit)` etc. — and the `// 2s is enough` comment sits on the line above the `sleep`, not inline. Faithful in spirit; note it's trimmed for legibility.)*

---

## Beat 5 — The "elegant" rewrite / false summit (3:45–4:45) · sell the win

**Point:** sell the clean fix as a triumph — set up *what it solved* clearly, because Act III reveals *what it broke*.

**Beats (confident, a victory lap with a faint shadow):**
- "Then I learned Claude's hooks can do more than notify — one kind can **block** and hand back structured data. So: stop impersonating a user. *Speak the agent's own protocol.*"
- "Intercept the question in the hook. Collect the answer in Telegram. Hand it straight back as JSON. No terminal. No keystrokes. No `Down, Down, Down`. No guessing how long a screen takes to paint."
- The payoff: "That ~4.7-second keystroke dance collapsed into building a tiny answer object — **microseconds** to assemble. I ripped out the whole keystroke dance — the `Down, Down, Down`, the `// 2s is enough`. It felt *fantastic*."
- The faint shadow (plant, don't explain): "It felt a little *too* clean. Hold that thought."

*(Honesty guardrails: "microseconds" = building the answer object, NOT end-to-end user-visible delivery — keep it scoped to "microseconds to assemble." And PR-E **modified** `callback_handlers.rs`, it didn't delete a file — say "ripped out the keystroke path," never "deleted the most fragile file.")*

**Visual:** before/after — the keystroke timeline bar vs a single dot labeled "µs to build the answer"; "keystroke path: removed ✓".

---

## Beat 6 — The rug-pull (4:45–5:30) · the turn

**Point:** the elegant fix violated the Beat-2 invariant. Reframe "the codebase taught me."

**Beats:**
- "Here's what the code taught me. Handing the answer back as data has a side effect I didn't see coming: it tells Claude *'this is already answered — don't ask.'* Which means it **stops drawing the question in the terminal at all.**" *(Full nuance for accuracy/Q&A: the rewrite also suppressed the question's `tool_start` event — the very signal that rendered it to Telegram in the working design — so the fix undercut the render on both sides. The CLI-suppression is the punchier on-stage version; the tool_start detail is the airtight one. ADR-015:27.)*
- "Remember the point — *one* conversation, either surface, **never switch contexts**? Here's the irony that took me a while to see. My fix forced the *exact* context-switch it was supposed to kill."
- The concrete sting: "Picture it: the question now lives **only** in Telegram. So if I'm sitting *right there at my keyboard* — agent's running in front of me — I can't answer it where I am. I have to stop, pick up my phone, and answer over *there*. I built this whole thing so I'd **never** have to switch interfaces… and my 'elegant' version *mandated* an interface switch. Every single time."
- Land it: "It didn't just drop a surface. It broke the **seamlessness** — the one continuous conversation — that is the entire reason the thing exists. The fast version wasn't faster. It was *narrower*. It was wrong."

**Visual:** the both-surfaces diagram with the terminal half greyed out / crossed.

---

## Beat 7 — The real fix (5:30–6:30) · sincere, the thesis

**Point:** Chesterton's fence. The fragile line wasn't at the wrong layer — it was *unverified*. Keep the mechanism; make it honest.

**Beats:**
- "So I put the keystrokes back. Injection isn't a regrettable hack here — it's the *one mechanism* that keeps the question on both screens. It was load-bearing the whole time."
- "But I fixed the line that was *actually* broken. Instead of guessing — `sleep 2 seconds, hope` — the code now *looks*. It reads the terminal (`tmux capture-pane`) and presses Enter the **instant** the review screen is actually there. Not before. Not on a timer."
- Honest numbers: "Typical case, that's a couple hundred milliseconds instead of a blind 2-second wait — and, more importantly, it **no longer blind-races the review screen**. The win wasn't a faster constant. The win was that it stopped *guessing* and started *checking*." *(Real constants: polls every 200ms, up to a 5-second cap. Don't cite "~150–450ms" — say "a couple hundred milliseconds, capped at five." Don't say "race-free / can't race" — ADR-015:135/137 records residual edge cases under a stale-but-visible widget; the honest claim is "no longer blind-races the review-screen timing.")*
- The pivot to demo: "Which means I can finally show you the thing I promised — for real."

**Visual:** `// 2s is enough` → `wait until the screen is actually there`; small "checks before it presses Enter · no more blind 2s guess."

---

## Beat 8 — LIVE DEMO (6:30–9:30) · the both-surfaces proof

**Setup:** laptop terminal (tmux, Claude Code running) and iPhone screen **side-by-side on the projector**. Mirror the iPhone via **QuickTime "New Movie Recording" over USB** (not AirPlay). tmux pane zoomed (`Ctrl+b z`), font ≥28pt, dark high-contrast theme, display mirrored at ~1280×720. Mac on the **iPhone's hotspot** (don't trust venue wifi). Telegram thread pre-opened. No tokens/`.env` visible anywhere on the path.

**The task (chosen for reliability): the phone makes the agent quiz me.**
1. From the **phone**, send a prompt into the session: *"Ask me one hard multiple-choice trivia question about programming history."* (Explicitly requesting a multiple-choice question is the most reliable way to force the AskUserQuestion tool to fire.)
2. The question renders **in both places at once** — the tmux terminal *and* Telegram, as tappable buttons. **This is the moment.** Narrate it: "Same question. Terminal *and* phone. One conversation."
3. Tap an answer **on the phone**. Watch the **terminal** visibly advance — the keystrokes injecting, the widget resolving. *Pause. Let it land.* "That selection just drove the real terminal. From my phone."
4. Claude reacts (right/wrong). Done.

**Narrate every action** — never type in silence. Total target ~2:00–2:30, leaving slack.

**Risk plan:** rehearse end-to-end ≥10× on the exact repo. A pre-recorded ≤90s screen capture of this exact flow sits one keystroke away in the deck. If anything stalls: *"While that reconnects, here's the identical run I recorded"* — show it, zero apology. (AskUserQuestion firing is non-deterministic; the recording is the insurance.)

**Visual asset to capture:** the ≤90s side-by-side recording (also doubles as the fallback).

---

## Beat 9 — Close (9:30–10:00) · sincere landing

**Three takeaways (reworded to engineering judgment; AI downplayed):**
1. **A fragile line isn't always at the wrong layer — sometimes it's just *unverified*.** Fix the verification before you fix the architecture.
2. **An elegant rewrite can optimize away the product.** Know what a line is load-bearing for before you delete it (Chesterton's fence).
3. **"Elegant" and "correct" are not the same measurement.** The clean version won the benchmark and lost the point; measure the regression, not just the speedup.

**The Channels line (one honest sentence, scoped, no version numbers — VERIFIED against official docs):**
> "Footnote: while I was deep in this, Anthropic shipped *Channels* — an official Claude Code bridge, to Telegram, the same platform I'd picked. Nice validation. But notice how the headless version handles the hard part: it just **turns the interactive questions off** so the session can't stall. Which is the honest tell — answering the question from your phone is hard enough that the official shortcut was to *not have the question*. I didn't want to give it up."

**Closing line (pre-written, deliver within 30s of demo, then STOP):**
> "The fix was never faster keystrokes. Sometimes the most fragile line in your system isn't in the wrong place — it just never bothered to check whether the thing it was waiting for had actually happened. Repo's on screen. Thanks."

**Visual:** repo URL + QR; name + LinkedIn small.

---

## Anchor numbers (verified in-repo / docs)

- **~20,000 lines of Rust** (src = 20,211 verified 2026-06-17; ~24.6k incl. tests). (Lean mention; full stats — 15 ADRs, ~595 tests — on the handout slide only.)
- **v1 (villain):** `KEY_DELAY` = 300ms/key; representative multi-select ≈ 9 keys ≈ ~2.7s of keystrokes **+ a blind `sleep(2000ms)`** ⇒ ~4.7s/answer, and the Enter could fire before the review screen rendered (unbounded race). Source: `dec6e30^:callback_handlers.rs` L1196–1218 (loop) + L1426/1431 (`auto_submit_answers`, `from_millis(2000)`, comment "2s is enough").
- **v2 (false summit, ADR-014 PR-E, shipped 0.2.19):** answer handed back via `hookSpecificOutput.updatedInput`; answer-object build ≈ microseconds (build time, NOT end-to-end delivery); keystroke path removed (the file was **modified, not deleted** — say "ripped out the keystroke path"). *Side effects:* `updatedInput` suppresses the CLI render (Claude Code #29547) AND PR-E suppressed the `tool_start` that drove the Telegram render (ADR-015:27) → question stopped showing in the terminal → broke the both-surfaces invariant.
- **v3 (real fix, ADR-015, on master through 0.2.22):** keystroke injection restored; blind sleeps replaced with `tmux capture-pane` readiness polling — current code: `READY_POLL_INTERVAL_MS`=**200**, `WAIT_STATE_MAX_POLLS`=**25** ⇒ **5000ms cap** (verified `callback_handlers.rs:7,17`). Enter only on the verified review screen. Typical ~a couple hundred ms; **no longer blind-races** the review-screen timing (residual stale-widget edge cases noted in ADR-015:135/137 — don't claim "race-free"). Win = correctness + adaptivity, not a tuned constant.
- **Channels (VERIFIED, official docs `code.claude.com/docs/en/channels`):** ships a Telegram plugin; in non-interactive `-p` mode, *"tools that need terminal input, such as multiple-choice questions and plan mode approval, are disabled so the session never stalls waiting for input"* (verbatim). Scope the talk claim to the **headless/Channels path** — do NOT generalize to "Anthropic's whole approach" (Remote Control is an ambiguous counter-case the docs don't resolve). No version numbers on slides.

---

## Visual assets to capture (your TODO before the deck is presentable)

1. **TUI widget screenshot** — real Claude Code AskUserQuestion multiple-choice menu in the terminal (Beat 3).
2. **≤90s side-by-side demo recording** — phone + terminal, the trivia flow (Beat 8 + fallback).
3. (I generate) the flow diagram, the keystroke-timeline-vs-µs visual, the `// 2s is enough` slide, the greyed-both-surfaces slide.

---

## Adapt for the room (mixed default)

- **More AI-eng in the room** → lean on Beats 5–7 (hooks, the `updatedInput` side effect, capture-pane).
- **More general SWE** → lean on Beats 4 + 6–7 + takeaways 1–2 (the wrong-layer trap, Chesterton's fence).
- **Leadership-heavy** → expand Beat 6 (the elegant fix that quietly broke the product = the review/governance story).
- **If Q&A is inside the 10 min:** cut Beat 5's victory-lap detail and the Channels footnote first.

---

## ⚠️ Repo follow-up flagged by Codex (NOT a talk task)

`docs/ARCHITECTURE.md` is **stale** — it still describes the reverted ADR-014 PR-E behavior (structured `updatedInput`, "no keystroke" AskUserQuestion) at lines 68, 145, 152. Since the repo is public and the talk gives the GitHub URL, an audience member could open `ARCHITECTURE.md` and find it contradicting both the talk *and* the shipped code. **Recommend updating `ARCHITECTURE.md` to reflect ADR-015** before the talk. The talk itself should cite ADR-015 / current code, never the architecture doc. (Say the word and I'll fix the doc — it's a real bug, separate from the talk.)

---

## Codex review (2026-06-17) — applied

Read-only `codex exec` review of this outline. Verdict: *"Use the outline; the core technical reversal is true."* Corrections applied: "deleted the file" → "ripped out the path"; current poll constants 200ms/5s cap (not 150/3s); "race-free" → "no longer blind-races"; "microseconds" scoped to answer-object build; "coin-flip" removed; LOC ~20k; "no answer API" scoped to the terminal-only input surface; added the `tool_start`-suppression nuance to Beat 6. Outstanding (above): stale `ARCHITECTURE.md`.

---

## Decisions log (locked with speaker)

- Arc: true 3-act reversal. Abstract left as-is; talk over-delivers past it.
- Demo: LIVE at end only; trivia-question-only; side-by-side terminal+phone; crafted prompt + rehearsal + recorded fallback.
- Open: earnest/story-driven; promise the demo up front; name one line at ~0:45.
- Voice: "I" (AI downplayed); reversal = "the codebase taught me"; punchy/laugh beats in the middle, sincere open & close; jargon + 4-word glosses.
- Beat 2: lean scale; one flow diagram; **plant the both-surfaces invariant**.
- Beat 3: real TUI screenshot; single villain = the multiple-choice question.
- Beat 4: real `send-keys` code on screen; wry; headline + one failure (Enter into a not-yet-rendered screen).
- Channels: one honest line in the close, scoped to headless, no version numbers (verified).
- Takeaways: the 3 new ones (#3 reworded to engineering judgment).
- Deliverable: Marp Markdown → single moderately-dense PDF (present + handout). **Build only after this outline is reviewed.**
