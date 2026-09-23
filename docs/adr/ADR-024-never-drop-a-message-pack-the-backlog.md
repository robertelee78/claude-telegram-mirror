# ADR-024: Never drop a message to Telegram — pace under the group limit and pack the backlog

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-23)
**Date:** 2026-09-23
**Authors:** Robert, Claude
**Tags:** telegram, delivery, rate-limiting, queue
**Related:** ADR-011 (rate limiting), ADR-014 (priority queue, A4 topic deletion), ADR-023 (handlers never wait on Telegram)

## Problem statement (the user's words)

> I sent the message from telegram, it got to the coding session, I got no error message
> in telegram, the agent session responded, its response did not go to telegram.

Telegram → coding session worked. Coding session → Telegram lost the agent's reply,
silently.

## Root cause

1. **Telegram allows a bot 20 messages a minute in a group.** Verbatim from the Bot API
   FAQ: *"In a group, bots are not able to send more than 20 messages per minute."* Every
   forum topic is the same group, so that is the whole mirror's budget.
2. **ctm read its `rate_limit` (default 20) as messages per *second*** — sixty times the
   ceiling — and its back-off floor was 0.5 msg/s, 30 a minute: *above* the ceiling, so
   no amount of backing off could ever get under it.
3. So the bot was refused continuously: **615 queue-wide "429, retry after 31–40 s"
   pauses on 2026-09-23 alone**, the send queue pinned at its cap (~320–340), and
   `Queue backpressure: evicted N oldest queued message(s)` every few seconds.
4. **A full queue deleted its oldest message.** Agent replies and tool-call chatter
   shared the tier. The reply the user waited for was deleted like any tool preview.
   Nothing was logged about *which* message went, so the loss was silent.

Inferred, not proven: that the specific 21:03 reply was among the evicted — the old code
did not record what it evicted. **Proven by reproduction:** `tests/daemon_budget.rs`, run
against the released 0.2.55 code with a stand-in Telegram enforcing the documented
limit, loses 3 of 5 agent replies under a tool-call flood.

Two more ways the same path lost messages: a message to a topic that had been deleted
got three tries and was thrown away (5268 "Topic not found" warnings in one log), and a
finished session's topic was deleted immediately (ADR-014 A4) even when its final reply
was still waiting to be sent.

## Decision

The user's rule: **don't drop responses — queue and retry.** And for the backlog:
**conjoin messages so each post carries more.**

1. **The budget is per minute.** `rate_limit` means messages per minute to the group
   (default 20, clamped 1–60). Posts are paced one slot at a time (`AimdState::book`),
   backing off on a 429 and never below a floor that is itself under the ceiling. Only
   calls that post into the group spend the budget; answering a button press, editing,
   fetching a file go straight out (they used to wait behind a per-call delay too).
2. **The outbox never drops for load.** A message leaves only when Telegram accepts it.
   429 → wait `retry_after`, try again. Network / 5xx → back off (2 s … 60 s), forever.
   The one give-up is a 400 refusing the *content* (after the plain-text fallback),
   logged with the text. A 50 000-item memory guard releases only tool chatter, loudly.
3. **A backlog is packed.** Each send takes the most urgent topic and packs everything
   waiting for it, in the order it happened, into one message up to 4 000 UTF-16 units
   (Telegram's 4 096 limit is in UTF-16 units; an emoji costs two). One post carries
   dozens of tool lines, so the budget moves hundreds a minute and the backlog drains.
   Approvals and questions keep their own message; tool previews' Details buttons
   survive merging, numbered to their lines. A lone message goes out unchanged.
4. **A vanished topic is held, not dropped.** Its messages are parked; the daemon makes
   a replacement topic (retrying under rate limits) and moves them there.
5. **A finished session's topic is deleted only once nothing is waiting for it** (up to
   10 minutes), off the handler, and not at all if the session resumes meanwhile.
6. **The outbox is saved to disk** (`<config>/outbox.json`, 0600, atomic) every 2 s while
   it changes and on shutdown, and reloaded on start.

## Proof

Against a stand-in Telegram that enforces the group limit (`tests/common`):

| Test | Before (0.2.55) | After |
|---|---|---|
| `daemon_budget` — 200 tool events + 5 replies at 20/min | 3 of 5 replies lost | all 5 replies and all 100 tool calls delivered in 7 posts, 0 refusals |
| `daemon_pacing` — 50 replies at once, 30/min | — | all 50 in 4 posts, 0 refusals |
| `daemon_topic_gone` — topic deleted mid-conversation | replies thrown away | replies land in a new topic |
| `daemon_outbox_restart` — daemon restarted with replies waiting | lost | delivered by the next daemon |

## Consequences

- Nothing the agent says is dropped because ctm or Telegram was busy. Under heavy load
  messages arrive packed and later, not missing.
- Posts during a backlog look different: several tool calls in one message, numbered.
- Existing configs with `rate_limit: 20` now mean what Telegram allows. A value above 60
  is clamped to 60 (one a second, the FAQ's single-chat ceiling).
- Still open (separate work): the inbound "Reply failed — tmux not detected" warning that
  fires when a message *was* delivered (ADR-013's submit check reads the bottom of the
  pane, where a just-sent or queued message legitimately still is).
