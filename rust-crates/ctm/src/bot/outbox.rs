//! ADR-024: the outbox — every message waiting for Telegram, held until it is sent.
//!
//! Reported: *"I sent the message from telegram, it got to the coding session … the
//! agent session responded, its response did not go to telegram."* Telegram allows a
//! bot 20 messages a minute in a group (Bot API FAQ), every forum topic is the same
//! group, and ctm produced far more than that — mostly tool-call chatter. So the bot
//! was refused all day, the queue filled, and a full queue deleted its oldest entry
//! to make room: an agent's reply went the same way as a tool preview, silently.
//!
//! The outbox replaces "delete when full" with two rules:
//!
//! 1. **Nothing is dropped for load.** Items leave only when Telegram accepts them.
//! 2. **When there is a backlog, one message carries many.** Everything waiting for a
//!    topic is packed, oldest first, into one message up to Telegram's size limit —
//!    one post can carry dozens of tool lines, so a 20-a-minute budget moves hundreds
//!    of lines a minute and the backlog drains instead of growing. With no backlog a
//!    message goes out on its own, exactly as before.
//!
//! Approvals and questions carry buttons the user must press on *that* message, so
//! they are never merged. Tool previews carry a "Details" button too; when they are
//! merged each keeps its button, numbered to match its line.
//!
//! A topic that turns out to be gone is **parked**, not dropped: its messages wait
//! until the daemon has made a replacement topic and [`Outbox::retarget`]s them.
//!
//! Everything here is pure and unit-tested; the drainer that uses it is in
//! `queue.rs`, and the whole path is proven against a stand-in Telegram that enforces
//! the real limit (`tests/daemon_budget.rs`).

use super::types::{InlineButton, MessagePriority, QueuedMessage};
use std::collections::{HashSet, VecDeque};

/// Separator between merged parts: a blank line, so each reads as its own paragraph.
const JOIN: &str = "\n\n";
/// Most buttons one merged message may carry. Telegram allows 100 per keyboard; a
/// merged message stays readable well below that.
const MAX_MERGED_BUTTONS: usize = 30;
/// Memory guard only. At Telegram's pace a backlog this deep is hours of traffic that
/// packing could not keep up with; past it the oldest *tool* items are released, and
/// said so loudly. Replies, questions and approvals are never released.
const HARD_CAP: usize = 50_000;

pub(super) struct Outbox {
    critical: VecDeque<QueuedMessage>,
    normal: VecDeque<QueuedMessage>,
    low: VecDeque<QueuedMessage>,
    next_seq: u64,
    /// Topics Telegram said no longer exist. Their messages are held, not sent.
    parked: HashSet<i64>,
    /// Tool items released by the memory guard. Reported, never silent.
    released: u64,
}

/// Length as Telegram measures it for the 4096 limit: UTF-16 code units, so an emoji
/// such as 🔧 costs two. Counting `char`s would let a post full of them run over.
fn tg_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Where a message goes: merging only ever happens within one topic.
fn topic_of(m: &QueuedMessage) -> (i64, Option<i64>) {
    (m.chat_id, m.thread_id)
}

/// Can this message be packed together with others?
///
/// Buttons the user must answer on this exact message (approvals, questions) — no.
/// A reply to a specific message — no, it must stay a reply. Tool previews' "Details"
/// buttons are Low priority and survive merging, renumbered.
fn mergeable(m: &QueuedMessage) -> bool {
    m.reply_to_message_id.is_none() && (m.buttons.is_none() || m.priority == MessagePriority::Low)
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Outbox {
    pub(super) fn new() -> Self {
        Self {
            critical: VecDeque::new(),
            normal: VecDeque::new(),
            low: VecDeque::new(),
            next_seq: 0,
            parked: HashSet::new(),
            released: 0,
        }
    }

    fn tier(&mut self, p: MessagePriority) -> &mut VecDeque<QueuedMessage> {
        match p {
            MessagePriority::Critical => &mut self.critical,
            MessagePriority::Normal => &mut self.normal,
            MessagePriority::Low => &mut self.low,
        }
    }

    /// Add a message. Stamps its arrival order, which is what keeps a topic's messages
    /// in the order they happened when they are packed together.
    pub(super) fn push(&mut self, mut msg: QueuedMessage) {
        msg.seq = self.next_seq;
        self.next_seq += 1;
        let p = msg.priority;
        self.tier(p).push_back(msg);
        if self.len() > HARD_CAP && self.low.pop_front().is_some() {
            self.released += 1;
            if self.released % 1000 == 1 {
                tracing::error!(
                    released_total = self.released,
                    cap = HARD_CAP,
                    "ADR-024: outbox memory guard — releasing the oldest tool previews; replies and approvals are kept"
                );
            }
        }
    }

    /// Put back a message that could not be sent yet, keeping its original place.
    pub(super) fn push_front(&mut self, msg: QueuedMessage) {
        let p = msg.priority;
        let tier = self.tier(p);
        let at = tier
            .iter()
            .position(|m| m.seq > msg.seq)
            .unwrap_or(tier.len());
        tier.insert(at, msg);
    }

    pub(super) fn len(&self) -> usize {
        self.critical.len() + self.normal.len() + self.low.len()
    }

    fn all(&self) -> impl Iterator<Item = &QueuedMessage> {
        self.critical
            .iter()
            .chain(self.normal.iter())
            .chain(self.low.iter())
    }

    fn sendable(&self, m: &QueuedMessage) -> bool {
        m.thread_id.is_none_or(|t| !self.parked.contains(&t))
    }

    /// How many messages are still waiting for this topic (parked or not). The daemon
    /// waits for this to reach zero before deleting a finished session's topic, so the
    /// agent's last reply is never deleted along with it.
    pub(super) fn pending_for(&self, thread_id: i64) -> usize {
        self.all()
            .filter(|m| m.thread_id == Some(thread_id))
            .count()
    }

    /// Telegram says this topic is gone: hold its messages until it is replaced.
    pub(super) fn park(&mut self, thread_id: i64) {
        self.parked.insert(thread_id);
    }

    pub(super) fn is_parked(&self, thread_id: i64) -> bool {
        self.parked.contains(&thread_id)
    }

    /// The daemon made a replacement topic: send everything that was waiting for the
    /// old one there instead. Returns how many messages moved.
    pub(super) fn retarget(&mut self, old: i64, new: i64) -> usize {
        self.parked.remove(&old);
        let mut moved = 0;
        for tier in [&mut self.critical, &mut self.normal, &mut self.low] {
            for m in tier.iter_mut().filter(|m| m.thread_id == Some(old)) {
                m.thread_id = Some(new);
                moved += 1;
            }
        }
        moved
    }

    /// The topic is gone *and* there is no session to make a new one for (it ended and
    /// ctm deleted the topic on purpose). Returns what was waiting, for the log.
    pub(super) fn discard_topic(&mut self, thread_id: i64) -> usize {
        self.parked.remove(&thread_id);
        let before = self.len();
        for tier in [&mut self.critical, &mut self.normal, &mut self.low] {
            tier.retain(|m| m.thread_id != Some(thread_id));
        }
        before - self.len()
    }

    /// The next message to send: everything waiting for the most urgent topic, packed
    /// oldest-first into one message of at most `max_chars`.
    ///
    /// The topic is chosen by priority (approvals and questions, then replies, then
    /// tool chatter). Within it, messages are taken in the order they happened — a
    /// reply is never posted above the tool calls that led to it — and packing stops at
    /// anything that must stand alone, a change of formatting, or the size limit.
    pub(super) fn next_batch(&mut self, max_chars: usize) -> Option<QueuedMessage> {
        let head = self
            .critical
            .iter()
            .chain(self.normal.iter())
            .chain(self.low.iter())
            .find(|m| self.sendable(m))?;
        let key = topic_of(head);

        // This topic's waiting messages, in the order they happened.
        let mut same: Vec<&QueuedMessage> = self.all().filter(|m| topic_of(m) == key).collect();
        same.sort_by_key(|m| m.seq);

        let first = same[0];
        let mut take: Vec<u64> = vec![first.seq];
        if mergeable(first) {
            let mut chars = tg_len(&first.text);
            let mut buttons = first.buttons.as_ref().map_or(0, Vec::len);
            for m in same.iter().skip(1) {
                if !mergeable(m) || m.parse_mode != first.parse_mode {
                    break;
                }
                let add = tg_len(JOIN) + tg_len(&m.text) + 4; // "#nn "
                let add_buttons = m.buttons.as_ref().map_or(0, Vec::len);
                if chars + add > max_chars || buttons + add_buttons > MAX_MERGED_BUTTONS {
                    break;
                }
                chars += add;
                buttons += add_buttons;
                take.push(m.seq);
            }
        }

        // Remove the chosen messages (in seq order) from wherever they sit.
        let mut parts: Vec<QueuedMessage> = Vec::with_capacity(take.len());
        for tier in [&mut self.critical, &mut self.normal, &mut self.low] {
            let mut i = 0;
            while i < tier.len() {
                if take.contains(&tier[i].seq) {
                    parts.push(tier.remove(i).expect("index in range"));
                } else {
                    i += 1;
                }
            }
        }
        parts.sort_by_key(|m| m.seq);
        Some(merge(parts))
    }

    /// Everything waiting, for saving to disk.
    pub(super) fn snapshot(&self) -> Vec<QueuedMessage> {
        let mut v: Vec<QueuedMessage> = self.all().cloned().collect();
        v.sort_by_key(|m| m.seq);
        v
    }

    /// Reload what a previous process left waiting. Arrival order is preserved and
    /// continues from there.
    pub(super) fn restore(&mut self, items: Vec<QueuedMessage>) {
        for m in items {
            self.push(m);
        }
    }
}

/// Pack messages (already in the order they happened, all for one topic) into one.
fn merge(mut parts: Vec<QueuedMessage>) -> QueuedMessage {
    if parts.len() == 1 {
        return parts.pop().expect("one part");
    }
    let numbered = parts.iter().filter(|m| m.buttons.is_some()).count() > 0;
    let mut text = String::new();
    let mut buttons: Vec<InlineButton> = Vec::new();
    let mut n = 0;
    for (i, m) in parts.iter().enumerate() {
        if i > 0 {
            text.push_str(JOIN);
        }
        match &m.buttons {
            Some(bs) if numbered => {
                n += 1;
                text.push_str(&format!("#{n} "));
                text.push_str(&m.text);
                for b in bs {
                    buttons.push(InlineButton {
                        text: format!("{} #{n}", b.text),
                        callback_data: b.callback_data.clone(),
                    });
                }
            }
            _ => text.push_str(&m.text),
        }
    }
    let first = &parts[0];
    QueuedMessage {
        chat_id: first.chat_id,
        text,
        thread_id: first.thread_id,
        buttons: (!buttons.is_empty()).then_some(buttons),
        parse_mode: first.parse_mode.clone(),
        // Notify if any part would have notified: a reply inside a batch of silent
        // tool chatter must still buzz the phone.
        disable_notification: parts
            .iter()
            .all(|m| m.disable_notification == Some(true))
            .then_some(true),
        reply_to_message_id: None,
        retries: 0,
        created_at: first.created_at,
        priority: parts
            .iter()
            .map(|m| m.priority)
            .min()
            .unwrap_or(first.priority),
        seq: first.seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str, thread: i64, p: MessagePriority) -> QueuedMessage {
        QueuedMessage {
            chat_id: -100,
            text: text.into(),
            thread_id: Some(thread),
            buttons: None,
            parse_mode: Some("Markdown".into()),
            disable_notification: None,
            reply_to_message_id: None,
            retries: 0,
            created_at: 0,
            priority: p,
            seq: 0,
        }
    }

    fn with_details(mut m: QueuedMessage, cb: &str) -> QueuedMessage {
        m.buttons = Some(vec![InlineButton {
            text: "Details".into(),
            callback_data: cb.into(),
        }]);
        m
    }

    #[test]
    fn nothing_is_dropped_for_load() {
        // The old queue held 300 and deleted the oldest past that. The outbox keeps
        // everything that can be sent until it is sent.
        let mut o = Outbox::new();
        for i in 0..5000 {
            o.push(msg(&format!("line {i}"), 1, MessagePriority::Low));
        }
        o.push(msg("THE REPLY", 1, MessagePriority::Normal));
        assert_eq!(o.len(), 5001);
        let mut seen = String::new();
        while let Some(b) = o.next_batch(4000) {
            seen.push_str(&b.text);
            seen.push('\n');
        }
        assert!(seen.contains("THE REPLY"));
        assert!(seen.contains("line 0") && seen.contains("line 4999"));
    }

    #[test]
    fn a_backlog_is_packed_into_few_messages() {
        let mut o = Outbox::new();
        for i in 0..200 {
            o.push(msg(
                &format!("Running: git status {i}"),
                7,
                MessagePriority::Low,
            ));
        }
        let mut posts = 0;
        while o.next_batch(4000).is_some() {
            posts += 1;
        }
        // ~25 chars a line: 200 lines fit in a handful of posts, not 200.
        assert!(posts <= 3, "{posts} posts for 200 lines");
    }

    #[test]
    fn a_lone_message_goes_out_unchanged() {
        let mut o = Outbox::new();
        o.push(msg("just this", 3, MessagePriority::Normal));
        let b = o.next_batch(4000).unwrap();
        assert_eq!(b.text, "just this");
        assert!(o.next_batch(4000).is_none());
    }

    #[test]
    fn a_topic_reads_in_the_order_things_happened() {
        // Tool chatter queued before a reply must appear above it in the same post,
        // even though the reply is higher priority.
        let mut o = Outbox::new();
        o.push(msg("tool A", 1, MessagePriority::Low));
        o.push(msg("tool B", 1, MessagePriority::Low));
        o.push(msg("the reply", 1, MessagePriority::Normal));
        let b = o.next_batch(4000).unwrap();
        let (a, bb, r) = (
            b.text.find("tool A").unwrap(),
            b.text.find("tool B").unwrap(),
            b.text.find("the reply").unwrap(),
        );
        assert!(a < bb && bb < r, "{}", b.text);
        assert_eq!(
            b.priority,
            MessagePriority::Normal,
            "carries the reply's urgency"
        );
    }

    #[test]
    fn the_most_urgent_topic_goes_first() {
        let mut o = Outbox::new();
        o.push(msg("chatter elsewhere", 9, MessagePriority::Low));
        o.push(msg("reply here", 1, MessagePriority::Normal));
        let b = o.next_batch(4000).unwrap();
        assert_eq!(b.thread_id, Some(1));
        assert!(!b.text.contains("chatter elsewhere"), "never mixes topics");
    }

    #[test]
    fn approvals_and_questions_stand_alone() {
        let mut o = Outbox::new();
        o.push(msg("before", 1, MessagePriority::Normal));
        let mut ask = msg("Approve?", 1, MessagePriority::Critical);
        ask.buttons = Some(vec![InlineButton {
            text: "Yes".into(),
            callback_data: "y".into(),
        }]);
        o.push(ask);
        o.push(msg("after", 1, MessagePriority::Normal));
        let texts: Vec<String> = std::iter::from_fn(|| o.next_batch(4000))
            .map(|b| b.text)
            .collect();
        assert_eq!(texts, vec!["before", "Approve?", "after"]);
    }

    #[test]
    fn merged_previews_keep_their_details_buttons_numbered() {
        let mut o = Outbox::new();
        o.push(with_details(
            msg("Bash: ls", 1, MessagePriority::Low),
            "d:1",
        ));
        o.push(with_details(
            msg("Read: a.rs", 1, MessagePriority::Low),
            "d:2",
        ));
        let b = o.next_batch(4000).unwrap();
        assert!(
            b.text.starts_with("#1 Bash: ls") && b.text.contains("#2 Read: a.rs"),
            "{}",
            b.text
        );
        let bs = b.buttons.unwrap();
        assert_eq!(bs.len(), 2);
        assert_eq!(bs[0].text, "Details #1");
        assert_eq!(
            bs[1].callback_data, "d:2",
            "each button still opens its own tool"
        );
    }

    #[test]
    fn packing_respects_the_size_limit() {
        let mut o = Outbox::new();
        for _ in 0..10 {
            o.push(msg(&"x".repeat(1500), 1, MessagePriority::Low));
        }
        while let Some(b) = o.next_batch(4000) {
            assert!(tg_len(&b.text) <= 4000);
        }
    }

    #[test]
    fn the_size_limit_is_counted_the_way_telegram_counts() {
        // 🔧 is one char but two UTF-16 units; Telegram's limit is in the latter.
        let mut o = Outbox::new();
        for _ in 0..40 {
            o.push(msg(&"🔧".repeat(100), 1, MessagePriority::Low));
        }
        while let Some(b) = o.next_batch(4000) {
            assert!(tg_len(&b.text) <= 4000, "{} units", tg_len(&b.text));
        }
    }

    #[test]
    fn a_reply_inside_silent_chatter_still_notifies() {
        let mut o = Outbox::new();
        let mut t = msg("tool", 1, MessagePriority::Low);
        t.disable_notification = Some(true);
        o.push(t);
        o.push(msg("reply", 1, MessagePriority::Normal));
        assert_eq!(o.next_batch(4000).unwrap().disable_notification, None);
    }

    #[test]
    fn a_missing_topic_parks_its_messages_until_they_are_rehomed() {
        let mut o = Outbox::new();
        o.push(msg("for the dead topic", 5, MessagePriority::Normal));
        o.push(msg("elsewhere", 6, MessagePriority::Normal));
        o.park(5);
        // Parked messages are held back, not dropped; other topics keep flowing.
        assert_eq!(o.next_batch(4000).unwrap().text, "elsewhere");
        assert!(o.next_batch(4000).is_none(), "held, not sent");
        assert_eq!(o.pending_for(5), 1);
        // A replacement topic arrives.
        assert_eq!(o.retarget(5, 50), 1);
        let b = o.next_batch(4000).unwrap();
        assert_eq!(
            (b.thread_id, b.text.as_str()),
            (Some(50), "for the dead topic")
        );
    }

    #[test]
    fn a_retried_message_keeps_its_place() {
        let mut o = Outbox::new();
        o.push(msg("first", 1, MessagePriority::Normal));
        o.push(msg("second", 1, MessagePriority::Normal));
        let b = o.next_batch(4000).unwrap(); // both, merged
        o.push(msg("third", 1, MessagePriority::Normal));
        o.push_front(b);
        let again = o.next_batch(4000).unwrap();
        assert!(again.text.starts_with("first"), "{}", again.text);
    }

    #[test]
    fn the_outbox_survives_a_restart() {
        let mut o = Outbox::new();
        o.push(msg("a", 1, MessagePriority::Low));
        o.push(msg("b", 2, MessagePriority::Normal));
        let saved = serde_json::to_string(&o.snapshot()).unwrap();
        let mut fresh = Outbox::new();
        fresh.restore(serde_json::from_str(&saved).unwrap());
        assert_eq!(fresh.len(), 2);
        assert_eq!(
            fresh.next_batch(4000).unwrap().text,
            "b",
            "priority survives"
        );
    }
}
