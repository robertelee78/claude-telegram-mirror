#!/usr/bin/env python3
"""STALE-TOPICS: enumerate every forum topic ID in the ctm channel via MTProto.

WHY THIS EXISTS
---------------
The Telegram *Bot* API (bot token) has NO method to list a forum's topics — it
can only create/edit/delete them, or learn an id reactively when a message
arrives (see deep-research: tdlib/telegram-bot-api#634). The *client* MTProto API
DOES expose `channels.getForumTopics`, but it requires an account session
(api_id/api_hash + your phone login), not a bot token. This script uses that
method to dump the complete topic-id list, which you then feed to:

    ctm prune-topics --ids topic_ids.txt

so the bot deletes exactly the stale topics — no blind id-range scanning.

ONE-TIME SETUP
--------------
1. Get a free api_id + api_hash at https://my.telegram.org  → "API development
   tools" (any app name/short-name works).
2. Install the client lib:   python3 -m pip install --user telethon
3. Run it (interactive — it logs into YOUR account, texts you a code):

     TG_API_ID=1234567 TG_API_HASH=abc... \
     python3 scripts/list_topics.py --chat -1003383453474 --out topic_ids.txt

   (Defaults: --chat is read from the ctm config if omitted; --out defaults to
    topic_ids.txt in the current directory.)

The session is cached in `ctm_topics.session` so re-runs won't re-prompt. Delete
that file to fully log out.

OUTPUT
------
One topic id per line in --out (suitable for `ctm prune-topics --ids`), plus a
human summary (id + title + closed flag) to stdout.
"""

import argparse
import json
import os
import sys
from pathlib import Path


def _import_telethon():
    """Import Telethon lazily so `--help` works before it is installed."""
    try:
        from telethon.sync import TelegramClient
        from telethon.tl.functions.channels import GetForumTopicsRequest

        return TelegramClient, GetForumTopicsRequest
    except ImportError:
        sys.exit(
            "telethon is not installed. Run:\n"
            "    python3 -m pip install --user telethon\n"
            "then re-run this script."
        )


def default_chat_id():
    """Read chatId from the ctm config so you don't have to pass --chat."""
    cfg = Path.home() / ".config" / "claude-telegram-mirror" / "config.json"
    try:
        d = json.loads(cfg.read_text())
        return d.get("chatId") or d.get("chat_id")
    except Exception:
        return None


def main():
    ap = argparse.ArgumentParser(description="List all forum topic IDs via MTProto.")
    ap.add_argument(
        "--chat",
        type=int,
        default=default_chat_id(),
        help="Supergroup/forum chat id (e.g. -100...). Defaults to ctm config.",
    )
    ap.add_argument("--out", default="topic_ids.txt", help="File to write topic ids to.")
    ap.add_argument(
        "--include-closed",
        action="store_true",
        help="Include already-closed topics (default: include everything).",
    )
    args = ap.parse_args()

    TelegramClient, GetForumTopicsRequest = _import_telethon()

    api_id = os.environ.get("TG_API_ID")
    api_hash = os.environ.get("TG_API_HASH")
    if not api_id or not api_hash:
        sys.exit(
            "Set TG_API_ID and TG_API_HASH (from https://my.telegram.org) in the "
            "environment, e.g.:\n"
            "    TG_API_ID=1234567 TG_API_HASH=abc... python3 scripts/list_topics.py"
        )
    if not args.chat:
        sys.exit("No --chat given and none found in ctm config.")

    # Session file caches your login so re-runs don't re-prompt.
    session = str(Path(__file__).resolve().parent / "ctm_topics")

    topics = []
    with TelegramClient(session, int(api_id), api_hash) as client:
        entity = client.get_entity(args.chat)

        # channels.getForumTopics paginates via (offset_date, offset_id, offset_topic).
        # We walk until a page returns no new topics. limit=100 is the server max.
        offset_date = 0
        offset_id = 0
        offset_topic = 0
        seen = set()
        while True:
            result = client(
                GetForumTopicsRequest(
                    channel=entity,
                    offset_date=offset_date,
                    offset_id=offset_id,
                    offset_topic=offset_topic,
                    limit=100,
                )
            )
            page = [t for t in result.topics if getattr(t, "id", None) is not None]
            new = [t for t in page if t.id not in seen]
            if not new:
                break
            for t in new:
                seen.add(t.id)
                topics.append(t)

            # Advance the cursor using the last topic on this page. The
            # ForumTopic carries top_message; messages are returned alongside.
            last = page[-1]
            offset_topic = last.id
            top_msg_id = getattr(last, "top_message", 0) or 0
            offset_id = top_msg_id
            # Find the date of that top message for offset_date.
            offset_date = 0
            for m in getattr(result, "messages", []):
                if getattr(m, "id", None) == top_msg_id:
                    offset_date = int(getattr(m, "date").timestamp()) if getattr(m, "date", None) else 0
                    break
            if len(new) < 100:
                break  # last (partial) page

    # The General topic is id=1 — never prune it; ctm also protects it, but we
    # omit it from the list so the file is purely candidates.
    ids = [t.id for t in topics if t.id != 1]
    if not args.include_closed:
        pass  # we list all; ctm decides liveness. (--include-closed kept for clarity.)

    Path(args.out).write_text("\n".join(str(i) for i in ids) + ("\n" if ids else ""))

    print(f"Found {len(topics)} topic(s) (excluding General id=1: {len(ids)} written to {args.out}).")
    print("id        closed  title")
    print("-" * 50)
    for t in sorted(topics, key=lambda x: x.id):
        closed = "yes" if getattr(t, "closed", False) else ""
        title = getattr(t, "title", "")
        flag = " (General)" if t.id == 1 else ""
        print(f"{t.id:<9} {closed:<6}  {title}{flag}")


if __name__ == "__main__":
    main()
