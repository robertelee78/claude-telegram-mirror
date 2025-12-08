# Telegram Bot Multi-Instance Polling Research

**Research Question**: Can multiple bot instances poll the same Telegram bot simultaneously on different machines?

**Short Answer**: **NO** - Multiple instances cannot use long polling (`getUpdates`) simultaneously with the same bot token. You will get `409 Conflict` errors.

---

## Table of Contents

1. [The Core Problem](#the-core-problem)
2. [Official Telegram API Documentation](#official-telegram-api-documentation)
3. [Why This Happens](#why-this-happens)
4. [Workarounds and Solutions](#workarounds-and-solutions)
5. [How Other Projects Handle This](#how-other-projects-handle-this)
6. [Implementation Recommendations](#implementation-recommendations)
7. [Your Project's Current Situation](#your-projects-current-situation)

---

## The Core Problem

### The 409 Conflict Error

When you run the same bot token with `getUpdates` (long polling) on multiple machines/processes simultaneously, you receive:

```json
{
  "ok": false,
  "error_code": 409,
  "description": "Conflict: terminated by other getUpdates request; make sure that only one bot instance is running"
}
```

**Root Cause**: Telegram's Bot API **explicitly prevents** multiple concurrent long polling connections for the same bot token. This is by design, not a bug.

**Source**: [Stack Overflow - 409 Conflict](https://stackoverflow.com/questions/69870914/conflict-terminated-by-other-getupdates-request-make-sure-that-only-one-bot-in)

---

## Official Telegram API Documentation

### getUpdates Method

From the [Telegram Bot API](https://core.telegram.org/bots/api):

> **getUpdates** - Use this method to receive incoming updates using long polling.
>
> **Important**: This method will not work if an outgoing webhook is set up.
>
> **Notes**:
> - Only one bot instance can call `getUpdates` at a time
> - In order to avoid getting duplicate updates, recalculate offset after each server response
> - Updates are confirmed once you call `getUpdates` with an offset that exceeds the update's ID

### Webhooks vs Long Polling

The API provides two mutually exclusive ways to receive updates:

1. **Long Polling** (`getUpdates`) - Bot pulls updates from Telegram
2. **Webhooks** (`setWebhook`) - Telegram pushes updates to your server

**Critical Constraint**: You can **only use one method at a time**. Setting a webhook disables `getUpdates` and vice versa.

**Source**: [Telegram Bots FAQ](https://core.telegram.org/bots/faq)

---

## Why This Happens

### Technical Explanation

1. **Offset Management**: The `getUpdates` method uses an `offset` parameter to track which updates have been processed. This offset is **shared state** managed by Telegram's servers.

2. **Connection Termination**: When Telegram receives a second `getUpdates` request for the same bot token:
   - It immediately terminates the first connection
   - Returns a 409 error to the second connection
   - This prevents update duplication and state corruption

3. **Host Memory**: "The telegram server may remember the host for some period of time and block same-token requests from other hosts" ([GitHub Issue #550](https://github.com/yagop/node-telegram-bot-api/issues/550))

### What About Sending Messages?

**Good News**: Multiple instances **can** send messages using the same bot token without conflicts.

> "Yes, multiple instances of your 'bot' can use the same bot token to send a message. The issue you can run into is a rate limit if your machines start sending too many messages at the same time."

**Source**: [Stack Overflow - Multiple Instances](https://stackoverflow.com/questions/71108015/telegram-bot-api-multiple-instances-of-same-bot)

The limitation is **only for receiving updates**, not sending messages.

---

## Workarounds and Solutions

### Solution 1: Webhooks with Load Balancing ✅ RECOMMENDED

**How it works**:
- Set one webhook URL that points to a load balancer
- Load balancer distributes incoming updates across multiple bot instances
- All instances can process updates in parallel

**Advantages**:
- True horizontal scaling
- Automatic failover
- Lower server load (no constant polling)
- Works with serverless platforms (AWS Lambda, Cloudflare Workers, etc.)

**Implementation**:

```javascript
// Set webhook (do this once, from any machine)
await bot.api.setWebhook('https://your-domain.com/bot-webhook', {
  max_connections: 100,  // Up to 100 concurrent connections
  drop_pending_updates: false  // Keep unprocessed updates
});

// On each server instance - handle webhook requests
app.post('/bot-webhook', async (req, res) => {
  const update = req.body;
  await bot.handleUpdate(update);
  res.sendStatus(200);
});
```

**Load Balancer Options**:
- NGINX
- AWS Elastic Load Balancer (ELB)
- Cloudflare Load Balancer
- Kubernetes Ingress
- Docker Swarm with routing mesh

**Sources**:
- [grammY: Long Polling vs Webhooks](https://grammy.dev/guide/deployment-types)
- [Scaling Telegram Bots](https://www.nextstruggle.com/how-to-scale-your-telegram-bot-for-high-traffic-best-practices-strategies/askdushyant/)

---

### Solution 2: Centralized Polling + Message Queue ✅ WORKS

**Architecture**:
```
┌─────────────────┐         ┌──────────────┐         ┌────────────┐
│   Telegram      │────────▶│   Polling    │────────▶│   Redis/   │
│   Bot API       │         │   Server 1   │         │  RabbitMQ  │
└─────────────────┘         └──────────────┘         └────────────┘
                                                             │
                               ┌─────────────────────────────┼────────────┐
                               ▼                             ▼            ▼
                          ┌─────────┐                  ┌─────────┐  ┌─────────┐
                          │Worker 1 │                  │Worker 2 │  │Worker N │
                          └─────────┘                  └─────────┘  └─────────┘
```

**How it works**:
1. **One** central server polls Telegram via `getUpdates`
2. Polling server publishes updates to a message queue (Redis, RabbitMQ, Kafka)
3. Multiple worker instances consume from the queue and process updates
4. All workers can send messages using the same bot token

**Advantages**:
- Works with existing long polling code
- No need for public HTTPS endpoint
- Easy to add/remove workers
- Can use simple VPS instead of serverless

**Example with Redis**:

```javascript
// Polling server (single instance)
const redis = require('redis');
const publisher = redis.createClient();

bot.on('message', async (ctx) => {
  await publisher.publish('telegram-updates', JSON.stringify(ctx.update));
});

bot.start();

// Worker servers (multiple instances)
const subscriber = redis.createClient();

subscriber.subscribe('telegram-updates');
subscriber.on('message', async (channel, message) => {
  const update = JSON.parse(message);
  await processUpdate(update);  // Your bot logic
});
```

**Sources**:
- [Python Telegram Bot Issue #2986](https://github.com/python-telegram-bot/python-telegram-bot/issues/2986)
- [Stack Overflow - Multiple Instances](https://stackoverflow.com/questions/71108015/telegram-bot-api-multiple-instances-of-same-bot)

---

### Solution 3: Send-Only Instances (No Polling) ✅ SIMPLE

**Use Case**: If your additional machines only need to **send** notifications, not receive messages.

**How it works**:
- Only one instance runs `bot.start()` and polls for updates
- Other instances just import the bot and use `bot.api.sendMessage()`
- No polling = no 409 errors

**Example**:

```javascript
// Machine 1 - Full bot with polling
import { Bot } from 'grammy';
const bot = new Bot(TOKEN);

bot.on('message', (ctx) => ctx.reply('Hello!'));
bot.start();  // Starts polling

// Machine 2 - Send-only (no polling)
import { Bot } from 'grammy';
const bot = new Bot(TOKEN);

// Don't call bot.start()!
// Just send messages directly
await bot.api.sendMessage(CHAT_ID, 'Notification from Machine 2');
```

**Advantages**:
- Dead simple
- No infrastructure changes needed
- No conflicts

**Limitations**:
- Only one machine can receive/respond to user messages
- Other machines are send-only

**Source**: [Stack Overflow - Sending Without Polling](https://stackoverflow.com/questions/71108015/telegram-bot-api-multiple-instances-of-same-bot)

---

### Solution 4: Local Bot API Server (Advanced) ⚠️

**What**: Run your own Telegram Bot API server instead of using `api.telegram.org`.

**Advantages**:
- Higher rate limits (up to 100,000 webhook connections)
- No file size limits (normally 20MB)
- Upload files up to 2000 MB
- More control over infrastructure

**Setup**:
```bash
git clone https://github.com/tdlib/telegram-bot-api.git
cd telegram-bot-api
mkdir build && cd build
cmake -DCMAKE_BUILD_TYPE=Release ..
make -j$(nproc)
```

**Configuration**:
```javascript
const bot = new Bot(TOKEN, {
  client: {
    apiRoot: 'http://localhost:8081'  // Your local API server
  }
});
```

**Sources**:
- [Telegram Bot API Server](https://github.com/tdlib/telegram-bot-api)
- [Bots FAQ - Local Server](https://core.telegram.org/bots/faq)

**Limitations**:
- Complex setup
- You must maintain the server
- Still doesn't solve multi-instance polling (you'd still need webhooks)

---

### Solution 5: Multiple Bot Tokens (Workaround) ⚠️

**Concept**: Create separate bots for each machine and distribute users across them.

**Not Recommended** because:
- Users need to add multiple bots
- Confusing UX
- Maintenance nightmare
- Doesn't actually solve the architectural problem

---

## How Other Projects Handle This

### grammY (Your Framework)

From [grammY deployment documentation](https://grammy.dev/guide/deployment-types):

> **Long Polling**: "Your bot only receives new messages every time it asks"
>
> **Webhooks**: "The Telegram servers will push them to your bot (via HTTP requests)"
>
> **Scaling**: Use the **runner plugin** for concurrent message processing within a single instance

**Key Quote**:
> "If you don't have a good reason to use webhooks, then note that there are no major drawbacks to long polling, and you will spend much less time fixing things."

**Multi-Instance Support**: grammY documentation doesn't explicitly cover multi-instance polling, **because it's not supported by Telegram**. They recommend webhooks for scaling.

---

### python-telegram-bot

From [GitHub Issue #2986](https://github.com/python-telegram-bot/python-telegram-bot/issues/2986):

**Developer Response**:
> "You cannot have more than one program calling `message_loop()` on the same token, because Telegram does not allow multiple processes to poll on the same token."

**Recommended Solution**: Use webhooks with load balancing or centralized polling with worker processes.

---

### node-telegram-bot-api

From [GitHub Issue #550](https://github.com/yagop/node-telegram-bot-api/issues/550):

**Common Causes**:
1. Multiple processes running
2. Bot restarted while previous instance still active
3. Manual `startPolling()` call when `polling: true` in constructor
4. Token used elsewhere (compromised or testing)

**Solution**: Kill all instances and ensure only one polls.

---

## Implementation Recommendations

### For Your Two-Machine Setup

Given your requirements (two machines, both need to receive updates):

#### Recommended: Webhooks + Simple Reverse Proxy

```
┌──────────────┐
│  Telegram    │
└──────┬───────┘
       │
       ▼
┌──────────────────┐
│  Cloudflare      │  (or any reverse proxy)
│  Load Balancer   │
└────────┬─────────┘
         │
    ┌────┴────┐
    ▼         ▼
┌────────┐ ┌────────┐
│Machine1│ │Machine2│
└────────┘ └────────┘
```

**Setup Steps**:

1. **Install a reverse proxy on one machine** (or use cloud service):
   ```bash
   # Using nginx on Machine 1
   apt install nginx
   ```

2. **Configure load balancing**:
   ```nginx
   upstream telegram_bot {
       server 192.168.1.10:3000;  # Machine 1
       server 192.168.1.11:3000;  # Machine 2
   }

   server {
       listen 443 ssl;
       server_name yourdomain.com;

       ssl_certificate /path/to/cert.pem;
       ssl_certificate_key /path/to/key.pem;

       location /webhook {
           proxy_pass http://telegram_bot;
       }
   }
   ```

3. **Update bot code to use webhooks**:
   ```javascript
   // Stop using bot.start() (long polling)
   // Instead, use webhook handler

   import express from 'express';
   import { webhookCallback } from 'grammy';

   const app = express();
   app.use(express.json());

   app.post('/webhook', webhookCallback(bot, 'express'));

   app.listen(3000, async () => {
     // Set webhook URL (do once)
     await bot.api.setWebhook('https://yourdomain.com/webhook');
     console.log('Webhook bot running on port 3000');
   });
   ```

4. **Ensure both machines run the same code**:
   - Both listen on port 3000
   - Both handle updates the same way
   - Load balancer distributes traffic

---

#### Alternative: Polling + Redis Queue (No Public HTTPS Needed)

If you can't set up a public domain/SSL:

```
Machine 1 (Poller)          Redis              Machine 2 (Worker)
┌─────────────┐         ┌─────────┐         ┌─────────────┐
│ getUpdates  │────────▶│  Queue  │◀────────│   Worker    │
│ (polling)   │         └─────────┘         │ (processing)│
└─────────────┘                             └─────────────┘
```

**Implementation**:

1. **Install Redis** (can be on either machine or separate):
   ```bash
   apt install redis-server
   ```

2. **Machine 1 - Polling Server**:
   ```javascript
   import { Bot } from 'grammy';
   import Redis from 'ioredis';

   const bot = new Bot(TOKEN);
   const redis = new Redis('redis://your-redis-server:6379');

   bot.on('message', async (ctx) => {
     // Push update to Redis queue
     await redis.rpush('telegram:updates', JSON.stringify(ctx.update));
   });

   bot.start();  // Only Machine 1 polls
   ```

3. **Machine 2 - Worker**:
   ```javascript
   import { Bot } from 'grammy';
   import Redis from 'ioredis';

   const bot = new Bot(TOKEN);  // For sending only
   const redis = new Redis('redis://your-redis-server:6379');

   // Don't call bot.start()! Just process queue

   async function processUpdates() {
     while (true) {
       const [, updateJson] = await redis.blpop('telegram:updates', 0);
       const update = JSON.parse(updateJson);

       // Process update
       await bot.handleUpdate(update);
     }
   }

   processUpdates();
   ```

**Advantages**:
- No public HTTPS required
- Easy to scale to more workers
- Failed updates can be retried

---

### Migration Path from Current Code

Your current setup uses **long polling**:

```javascript
// src/bot/telegram.ts line 242
// Start long polling
this.bot.start({
  onStart: (botInfo) => {
    logger.info(`Bot started: @${botInfo.username}`);
  }
});
```

**To avoid 409 conflicts**, you must choose ONE of:

1. **Only run daemon on one machine** (simplest short-term fix)
2. **Switch to webhooks** (best long-term solution)
3. **Implement polling + queue** (works without public domain)

---

## Your Project's Current Situation

### Code Analysis

From `/opt/claude-mobile/packages/claude-telegram-mirror/src/bot/telegram.ts`:

- **Framework**: grammY
- **Mode**: Long polling (`bot.start()` at line 243)
- **Issue**: README already warns about this (line 396):
  ```bash
  # Check for multiple bot instances (409 error)
  pkill -f "node.*dist/cli"
  ```

### Current Architecture

```
Machine 1                    Machine 2
┌─────────────┐             ┌─────────────┐
│ Daemon      │             │ Daemon      │
│ (polling)   │             │ (polling)   │  ← 409 CONFLICT!
└─────────────┘             └─────────────┘
      │                           │
      └───────────┬───────────────┘
                  ▼
            Telegram Bot API
```

Both daemons call `getUpdates` → **Conflict**

### Recommended Fix

**Option A: Single Daemon (Quick Fix)**
- Run daemon only on one machine
- Other machine sends messages via API (no polling)
- Simplest solution if you don't need both machines to receive messages

**Option B: Webhooks (Proper Solution)**
- Set up HTTPS endpoint (use Cloudflare Tunnel if no public IP)
- Convert to webhook mode
- Both machines can handle updates
- Production-ready scaling

**Option C: Polling + Queue (No HTTPS Required)**
- Run Redis on one machine
- One daemon polls, pushes to Redis
- Both machines consume from queue
- Good middle ground

---

## Testing and Validation

### Test 1: Verify 409 Error

```bash
# Terminal 1 on Machine 1
node dist/cli.js start

# Terminal 2 on Machine 2
node dist/cli.js start

# Expected: Machine 2 gets 409 error
```

### Test 2: Verify Send-Only Works

```bash
# Machine 1 - Full daemon
node dist/cli.js start

# Machine 2 - Send message only
node -e "
import { Bot } from 'grammy';
const bot = new Bot(process.env.TELEGRAM_BOT_TOKEN);
await bot.api.sendMessage(process.env.TELEGRAM_CHAT_ID, 'Test from Machine 2');
console.log('Sent!');
"
```

### Test 3: Webhook Setup

```bash
# Set webhook
curl -X POST "https://api.telegram.org/bot${TOKEN}/setWebhook" \
  -d "url=https://yourdomain.com/webhook"

# Verify webhook
curl "https://api.telegram.org/bot${TOKEN}/getWebhookInfo"

# Remove webhook (go back to polling)
curl -X POST "https://api.telegram.org/bot${TOKEN}/deleteWebhook"
```

---

## Sources

### Official Documentation
- [Telegram Bot API](https://core.telegram.org/bots/api)
- [Telegram Bots FAQ](https://core.telegram.org/bots/faq)
- [grammY Deployment Types](https://grammy.dev/guide/deployment-types)

### Community Discussions
- [Stack Overflow: 409 Conflict Error](https://stackoverflow.com/questions/69870914/conflict-terminated-by-other-getupdates-request-make-sure-that-only-one-bot-in)
- [Stack Overflow: Multiple Bot Instances](https://stackoverflow.com/questions/71108015/telegram-bot-api-multiple-instances-of-same-bot)
- [GitHub: node-telegram-bot-api #550](https://github.com/yagop/node-telegram-bot-api/issues/550)
- [GitHub: python-telegram-bot #2986](https://github.com/python-telegram-bot/python-telegram-bot/issues/2986)
- [GitHub: TelegramBots #1221](https://github.com/rubenlagus/TelegramBots/issues/1221)

### Scaling Resources
- [How to Scale Your Telegram Bot](https://www.nextstruggle.com/how-to-scale-your-telegram-bot-for-high-traffic-best-practices-strategies/askdushyant/)
- [The Ultimate Guide to Telegram Bot Webhooks](https://sirvelia.com/en/telegram-bot-webhook/)
- [Stack Overflow: Architecture for 150k Users](https://stackoverflow.com/questions/58829977/architecture-of-telegram-bot-for-150k-users-simultaneously)

---

## Conclusion

**Answer to Original Question**: No, multiple bot instances cannot poll simultaneously with `getUpdates`. This is a **fundamental limitation** of Telegram's Bot API, not a bug or library issue.

**Solutions Exist**:
1. ✅ Webhooks + Load Balancer (best for production)
2. ✅ Polling + Message Queue (best for private servers)
3. ✅ Send-only instances (simplest if one-way is enough)
4. ❌ Multiple bot tokens (poor UX, not recommended)

**For Your Two-Machine Setup**:
- **Quick Fix**: Run daemon on one machine only
- **Proper Fix**: Switch to webhooks or implement polling + Redis queue

All solutions are battle-tested and used by large-scale Telegram bots. The choice depends on your infrastructure and requirements.
