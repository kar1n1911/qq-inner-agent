# QQ Inner Agent

> [简体中文](README_zh.md) | **English**

A persistent conversational agent for one QQ account, connected through NapCat or SnowLuma's forward OneBot v11 WebSocket. Each enabled group and private contact has its own context, memory, and pool of candidate contributions. DeepSeek is preconfigured; OpenAI-compatible Chat Completions and Anthropic-compatible Messages endpoints are supported.

## Overview

- One QQ account, with explicitly enabled groups and private contacts.
- An observation period before first group participation, followed by a model-selected initial style.
- Candidate generation, evaluation, sending probability and response expectations.
- Isolated long-term notebooks, short-term details and traits for groups and people.
- A Chinese/English web dashboard with live configuration, logs and QQ diagnostics.
- DeepSeek presets and OpenAI-/Anthropic-compatible API adapters.

## Contents

- [Quick start](#quick-start)
- [Bridge connection](#bridge-connection)
- [Web dashboard](#web-dashboard)
- [Configuration](#configuration)
- [Conversation behavior](#conversation-behavior)
- [Operation and local data](#operation-and-local-data)
- [Troubleshooting and verification](#troubleshooting-and-verification)
- [Research background](#research-background)

## Quick start

### Requirements

- Linux with Node.js 22.13+; set `AGENT_NODE` if its executable is not on your path. The agent has no npm dependencies.
- Python 3 for setup and service-installation commands; systemd user services for background operation.
- One QQ account logged in through NapCat or SnowLuma, with a forward OneBot v11 WebSocket server. See [Bridge connection](#bridge-connection).
- A DeepSeek or compatible provider API key, plus the group/private-contact IDs you want to enable.

The agent needs no elevated permissions. Keep credentials in local setup or the dashboard, not in the repository.

### Configure and start

In a terminal:

```bash
cd qq-inner-agent
./agent setup          # enter the API key without terminal echo; choose chat IDs
./agent contacts       # list IDs after saving the bridge settings; rerun setup to change selection
./agent check --api    # verify QQ and a small model request; sends no QQ message
./agent install-service
./agent status
```

Install the background service with `./agent install-service`. Setup restarts an installed service after saving changes. Before a key and chat IDs are entered, the running agent keeps trying to connect to the QQ bridge but does not invoke the model or send QQ messages. Selecting a chat enables its new messages to be processed by the configured model provider. The group observation step may fetch a bounded history sample for initial analysis; it does not replay that sample as new messages.

QQ must be logged in through the chosen bridge, and its OneBot WebSocket server must be running. The agent reconnects when these recover; it cannot log QQ in or repair the bridge itself.

## Bridge connection

Both bridges use the same forward OneBot v11 WebSocket adapter. Configure one bridge at a time.

### NapCat

1. Start NapCat and sign in to QQ. In NapCat WebUI, open **Network configuration → New → WebSocket server** (正向 WebSocket). The agent connects as a client; reverse WebSocket and HTTP-only endpoints are not supported.
2. Enable the server on port `3001`, with **message format `array`**, a nonempty access token, and host `127.0.0.1` when both programs run directly on the same host. Keep event pushing enabled. The equivalent server entry is in [examples/napcat-websocket-server.json](examples/napcat-websocket-server.json); add it to NapCat's `network.websocketServers` list, replacing the token placeholder. It is a single server entry, not a complete NapCat configuration.
3. Run `./agent setup` and enter `ws://127.0.0.1:3001/`, the same OneBot token, and optionally the QQ account ID. A blank account ID in the dashboard (or `-` in the wizard) detects the logged-in account. Alternatively, edit these under **Configuration → NapCat / SnowLuma** in the dashboard. Save before loading contacts.
4. Run `./agent check` to verify the bridge and `./agent contacts` to list available IDs. Select chats and enter the model API key before starting participation.

For NapCat in Docker with the agent on the host, bind the WebSocket server to `0.0.0.0` **inside the container** and publish `127.0.0.1:3001:3001`. The agent still connects to `ws://127.0.0.1:3001/`. For another machine, use a TLS WebSocket proxy (`wss://`) or an SSH tunnel to a local port. Plain `ws://` is accepted only for localhost. Port `3000` is normally HTTP, and NapCat's WebUI port is not the OneBot WebSocket endpoint.

Use the root WebSocket path `/`, which carries both actions and events; NapCat's `/api` path does not deliver events. Both array and CQ-string message events are accepted; outgoing content uses escaped text segments, optionally followed by an allowlisted native face segment. Self-message reports are ignored. NapCat and SnowLuma need separate ports if running together; configure the agent for one bridge at a time.

Compatibility is based on NapCat's [network documentation](https://doc.napneko.icu/onebot/network), [configuration schema](https://github.com/NapNeko/NapCatQQ/blob/main/packages/napcat-onebot/config/config.ts), and [WebSocket implementation](https://github.com/NapNeko/NapCatQQ/blob/main/packages/napcat-onebot/network/websocket-server.ts). Automated mock tests cover authentication, login/status, contacts, mentions, private replies, self-message filtering, and both event formats. A live NapCat account has not yet been used for end-to-end verification.

### SnowLuma

Sign in to QQ, load its process in SnowLuma, and enable the forward OneBot WebSocket server. Enter its URL and access token in `./agent setup` or **Configuration → NapCat / SnowLuma**, then run `./agent check`. Use the bridge's configured port; do not assume it matches NapCat's example port. The agent cannot repair QQ login or component-loading failures in the bridge itself.

## Web dashboard

### Install and open

```sh
./agent install-dashboard   # Generate certificates and install/refresh the user service
./agent dashboard-key       # Display the private login key locally
```

Use `systemctl --user status qq-inner-dashboard.service` to check the dashboard service and `systemctl --user restart qq-inner-dashboard.service` to restart it. To run the dashboard in the foreground instead, stop its service first and run `./agent dashboard`.

`dashboard.json` contains bind addresses, permitted origins, and TLS paths. Dashboard-server changes require a dashboard restart. Agent behavior settings reload live. Secrets/configuration saves use a recovery journal so an interrupted two-file write can roll back when the dashboard restarts.

### Access and configuration

The installed dashboard runs independently of the agent:

- Local host: http://localhost:5097
- LAN: `https://<host-LAN-IP>:5098`
- VPN: `https://<host-VPN-IP>:5098`

On a remote device, import the public `dashboard-ca.crt` into its trusted certificate authorities before opening the HTTPS address. Keep `data/dashboard-ca.key` and all other private keys on this host. The certificate covers the addresses available during installation; run `./agent install-dashboard` again if addresses change. Remote connectivity still depends on your LAN/VPN routing and firewall. The HTTP listener is loopback-only. A plain `http://` request to the dashboard port is answered with a redirect to the same address over HTTPS, so only the TLS listener needs to be reachable from a remote device.

Retrieve your login key on the host with `./agent dashboard-key`. Sessions expire after 12 hours. HTTPS, HttpOnly cookies, CSRF checks, an origin allowlist, and login rate limiting protect remote access. Do not expose this private administration service to the public internet. The dashboard key gives access to configuration, retained conversation ideas, operational logs, and agent controls.

In **Configuration**, enter your DeepSeek key, choose the API format/model, and enable the desired group/private IDs. **Load QQ contacts** can populate the selection from the signed-in QQ account. API keys remain blank after saving; the interface only indicates whether a key is configured. Choose **Save & apply** to reload the agent. In-flight generation is cancelled when its configuration becomes obsolete; stored memory remains. The application reports when the saved revision has been applied. A provider-host change requires a replacement key or explicit clearing of the old key.

**Overview** shows service, QQ and model status, readiness, and recent decisions. **Activity & logs** shows retained candidate contributions and refreshes operational logs every two seconds. **Test API connection** makes one small request using the saved provider key and counts against the configured API budget; it sends no QQ message. Start, stop, and restart control only the agent service, leaving the dashboard available.

### QQ send/receive diagnostics

The Overview page includes two QQ diagnostics, independent of the model and chat allowlists:

- **Send self-test message** sends one uniquely marked text message to the QQ account returned by the bridge's login API. A successful result means the bridge accepted the action; check QQ's self-chat to confirm delivery. The recipient and message cannot be overridden. Failed or uncertain sends are not retried automatically.
- **Start receiving test** listens for 60 seconds. From that same QQ account, send several messages (text, image, voice, file, reply) to yourself or another chat. Enable **reportSelfMessage** on the bridge's WebSocket server first; some bridges do not emit self-chat events even when sending succeeds. The viewer accepts `message` and `message_sent` events only when both the account and sender match the logged-in account. It displays the last 30 events, text previews and segment types; it does not fetch attachments, invoke the AI, or write captures to disk. **Stop receiving** closes the diagnostic connection early. Starting a new test clears the previous capture.

If receiving stays empty, check self-message reporting and that the URL carries events (use `/`, not `/api`). An empty capture is not a successful receiving test. Diagnostics use the saved connection settings, so save URL/token changes first. They can run while the agent service is stopped.

## Configuration

### Provider and model

The wizard offers `deepseek-openai`, `deepseek-anthropic`, and `custom`. The current DeepSeek preset uses `deepseek-flash`; you can enter another model available to your API account. The two DeepSeek profiles use the same API key.

| Profile | API base URL | Request format |
| --- | --- | --- |
| DeepSeek OpenAI | `https://api.deepseek.com` | `POST /chat/completions`, bearer authentication |
| DeepSeek Anthropic | `https://api.deepseek.com/anthropic` | `POST /v1/messages`, `x-api-key` and `anthropic-version` |
| OpenAI-compatible | Your base URL, typically ending in `/v1` | `POST /chat/completions` relative to the base |
| Anthropic-compatible | Your base URL | Appends `/v1/messages`, or `/messages` when base ends in `/v1` |

A full endpoint ending in `/chat/completions` or `/messages` is also accepted. `provider.tokenParameter` selects `max_tokens` for DeepSeek/older gateways or `max_completion_tokens` for OpenAI. `provider.anthropicAuth` supports `x-api-key` or `bearer`; `workspaceId` is available for Anthropic keys that require it. The DeepSeek presets disable extended thinking to keep the short structured stages responsive. The application reads only final text, not provider reasoning fields.

The adapters follow the [OpenAI Chat Completions reference](https://developers.openai.com/api/reference/python/resources/chat/subresources/completions/methods/create), [Anthropic Messages guide](https://platform.claude.com/docs/en/build-with-claude/working-with-messages), and [DeepSeek API documentation](https://api-docs.deepseek.com/). Model availability and gateway parameter support depend on the provider.

In **Choose a model**, click **Load available models** after saving your provider URL, API format, and key. Select a returned ID or enter one manually in **Model & API**, then **Save & apply**. Listing models does not generate a completion. The list uses the provider's first returned page (up to 500 IDs); gateways without a model-list API and models omitted from that page can use manual entry. DeepSeek uses its [models endpoint](https://api-docs.deepseek.com/api/list-models/) for both API formats; Anthropic-compatible providers use their [Messages API model-list route](https://platform.claude.com/docs/en/api/models).

### Language / 语言设置

控制台默认使用简体中文。页面顶部和“配置 → 语言设置”可切换 **简体中文 / English**；登录前的选择保存在当前浏览器，登录后使用已保存的界面语言。登录后点击“保存并应用”可保存界面和回复语言。回复语言可选 **跟随聊天语言、简体中文、English**，与界面语言相互独立。

候选生成、评分和最终发言的系统提示词均使用中文；切换英文界面不会把提示词改成英文。JSON 字段名、协议标识、模型 ID 和原始日志保持不变。默认角色说明也改为中文，升级时仅替换原版英文默认角色说明，自定义角色设定保持原样。

The dashboard defaults to Simplified Chinese, with an English option. Save interface and reply-language preferences under Configuration → Language settings. System prompts remain Chinese for either interface language. Replies can follow the conversation or use a fixed Chinese/English language. Existing custom personas and conversation content are not translated.

### Active and inactive times

In **Configuration → Active and inactive times**, enable the daily schedule, choose **Active from**, **Inactive from**, and an IANA time zone (for example `Europe/Stockholm`). The active interval includes its start and excludes its end. Overnight windows such as 22:00–06:00 work too. Disable the schedule for 24-hour availability; equal start/end times are rejected. With the activity rhythm below disabled, while inactive the agent skips incoming messages and pauses all automatic replies, including mentions and private messages. Queued work is discarded at the cutoff, and an in-flight response cannot be sent after inactive hours begin. Connections and manual diagnostics remain available. Quiet hours are a separate restriction on proactive replies within active hours.

### Continuous activity rhythm / 连续活跃与休息节奏

**Configuration → Continuous activity rhythm** enables probabilistic availability in place of the strict daily schedule gate. It is off by default to preserve existing schedules. All chats share one account-wide active/rest block, including direct mentions and private replies. The state and expiry persist in SQLite; polling, messages, and restarts do not reroll an unexpired block. Duration is sampled uniformly within the configured range. Adjacent blocks can choose the same state, so an uninterrupted run can exceed one block's maximum duration.

| Setting (`agent.rhythm.*`) | Default | Meaning |
| --- | --- | --- |
| `dayProbability` | 0.85 | Chance of choosing an active block during the active window |
| `edgeProbability` | 0.65 | Active-block probability at the edges of the inactive window |
| `centerProbability` | 0.02 | Active-block probability at the middle of the inactive window |
| `sigma` | 0.22 | Gaussian width relative to the entire inactive interval |
| `activeMinSeconds` / `activeMaxSeconds` | 300 / 1200 | Active blocks last 5–20 minutes |
| `restMinSeconds` / `restMaxSeconds` | 600 / 2400 | Rest blocks last 10–40 minutes |

During inactive hours, let `x` be progress from 0 to 1 through that interval in the schedule's local time, wrapping midnight. With `g(x) = exp(-0.5 × ((x − 0.5) / sigma)²)`, activity probability is `edge − (edge − center) × (g(x) − g(0)) / (1 − g(0))`. This is a bounded Gaussian-shaped **inactivity** curve: rest is most likely at the center, and activity rises toward either edge. It is a heuristic, not a measured model of human behavior. These are probabilities of selecting a block, not per-message send rates or the fraction of clock time spent active; different active/rest durations affect that fraction.

Blocks keep their state across daily schedule boundaries and use absolute elapsed seconds for their duration. Time-zone/DST changes affect the next selection's local-time curve. After a long offline gap, the agent samples one new block on resuming instead of replaying missed blocks. Editing rhythm or schedule settings starts a new block; unrelated settings preserve it. Disabling the daily schedule uses `dayProbability` all day. Disabling rhythm restores the original strict schedule.

Rest skips incoming messages and discards queued replies. A response whose generation crosses a block boundary is withheld, even if the next block is active. Becoming active does not trigger a greeting or catch up missed messages. When active, observation, quiet hours, forecasts, per-message sending probabilities, cooldowns and budgets still apply. Quiet hours can therefore suppress proactive messages even during a randomly active nighttime block. The Overview page displays current state, next selection time, sampled probability and current curve probability. No additional model requests are needed.

### Participation limits and live updates

Use the dashboard to save and apply settings live, or edit `config.json` locally. The agent checks for changes every second; invalid edits leave the previous configuration running. [`config.example.json`](config.example.json) provides a starting configuration; [`src/config.mjs`](src/config.mjs) defines the full defaults and validation rules.

| Setting | Default | Effect |
| --- | --- | --- |
| `agent.proactive` | `true` | Consider unsolicited contributions in enabled chats |
| `agent.threshold` | `4.09` / 5 | Motivation required on open turns |
| `agent.interruptThreshold` | `4.8` / 5 | Motivation required when another person is addressed |
| `agent.system1Probability` | `0` | Optional chance of a brief low-motivation acknowledgment |
| `agent.proactiveCooldownSeconds` | `180` | Minimum interval between proactive contributions |
| `agent.maxProactivePerHour` | `6` | Per-chat proactive limit |
| `agent.maxMessagesPerHour` | `30` | Per-chat total output limit, including replies |
| `agent.pauseSeconds` | `45` | Pause trigger after human activity |
| `agent.activeWindowSeconds` | `900` | Never revive inactive chats after this window |
| `agent.quietHours` | `23:00–08:00`, Europe/Stockholm | Suppress unsolicited contributions; set `null` to disable |
| `agent.dryRun` | `false` | Evaluate and record decisions without sending |
| `provider.requestsPerHour` | `120` | Global rolling API-call limit, including retries |

Direct messages in enabled private contacts, explicit QQ @mentions, and an alias followed by `:`/`：` address the agent. Direct replies bypass the proactive cooldown and quiet hours, but not the total message/API budgets. Model-inferred invitations cannot bypass those safeguards. Other bot accounts can be listed in `agent.ignoredUsers`.

## Conversation behavior

### Group observation / 入群观察期

Observation is enabled by default for allowlisted groups, including groups enabled before this upgrade. It starts when the agent first registers that group, or restarts after receiving a new self-account `group_increase` join notice. Duplicate join notices do not restart it twice. State and the chosen style persist across agent restarts. Private chats bypass this gate.

Before the first group reply (including an @mention), both **300 elapsed seconds and 20 unique new human messages** are required by default. **Configuration → Group observation period** changes these thresholds, chooses both/either, or disables observation. Time is wall-clock time since registration; existing activity schedules still govern which incoming messages are processed. Historic samples, self echoes, replayed events, ignored users and unrelated chats do not count. If a group stays quiet, observation may wait indefinitely for enough new messages. Crossing a threshold never forces a greeting; the usual activity window and sending policy still apply.

The agent requests `get_group_info`, `_get_group_notice` and `get_group_msg_history` through the current OneBot connection. These are listed in [NapCat's API compatibility documentation](https://doc.napneko.icu/develop/api); other bridges may not implement the extension endpoints. The collector bounds group metadata, up to five text announcements, and a configurable history sample (default 30). Unsupported/failed/malformed sources are recorded as unavailable, not fabricated; available live chat can still support a conservative initial style. Images and file URLs are not fetched. Historical samples are analysis-only and are not replayed into the sending engine or counted toward the threshold. Collected history/announcements are removed after the configured raw-data retention window while the selected style remains.

Once the thresholds pass, a separate Chinese `ORIENT` request analyzes the supplied group context and selects an initial speaking style, a short overview, and topics. It uses the existing provider/API budget. Invalid output or provider failure holds the gate closed and retries after 60 seconds. Successful analysis is required before formation/evaluation/articulation; all those stages receive the chosen style as advisory context. This adds one model request per successful initial orientation, plus any failed attempts; later conversational learning can refine the style. Group metadata and announcements remain untrusted quoted content.

**Activity & logs → Group observation period** displays elapsed time, message count, source availability and the selected style. Metadata collection begins on the first eligible group processing cycle. No real QQ history retrieval or model analysis was exercised during automated verification.

### Sending probability and expectations / 发送策略与预测

In **Configuration → Sending policy & predictions** (配置 → 发送策略与预测), configure the new sending check; it is enabled by default. Existing schedule, quiet hours, cooldowns and rate limits still apply. A selected candidate receives a Chinese `FORECAST` request before articulation: whether to send, probabilities of a normal reply / silence / negative reaction, and a short response plan. Invalid forecasts fail closed. This adds one model request to an eligible cycle (four requests for a successful full cycle), charged against the existing API budget.

The proactive admission probability is the product of:

- Base probability (default 0.8).
- Settling factor: `min(1, seconds since latest human arrival / 15)`.
- Recovery factor: `min(1, seconds since last delivery attempt counted by policy / 300)`; 1 when none exists.
- Pace factor: `1 / (1 + human messages in the last minute / 6)`.
- Motivation factor: `0.25 + 0.75 × (adjusted score − 1) / 4`.
- Forecast factor: `1 − predicted negative reaction probability`.

The time divisors, pace divisor and base probabilities are configurable. Timing is sampled before the forecast request so model latency does not inflate the chance to speak. Direct requests use their separate base probability (default 1) without these reductions. Both modes are vetoed when the forecast says to wait or the negative-reaction probability exceeds its configured ceiling (default 0.4). The probability draw is persisted once per triggering human message: a rejected attempt does not roll again on a silence timer or after a restart. Disabling this check restores the previous selection flow.

Articulation uses the response plan. Only a confirmed send establishes an expectation, retained per chat for 300 seconds by default. The next turn receives that forecast plus elapsed time and whether another human message arrived; arrival alone does not prove a reply or agreement. Expectations do not schedule automatic follow-ups. **Activity & logs → Sending forecasts** displays the probability, draw, timing factors, outcome forecast, plan and delivery status. Predictions are subjective estimates, not calibrated probabilities or a reproduction of the paper's experiments.

### Adaptive persona and memory / 自适应角色与记忆

The default Chinese persona now emphasizes picking up conversational threads, light associations and humor, and starting relevant, easy-to-answer topics without constant questions or forced activity. Exact previous default personas migrate automatically; custom personas remain unchanged.

The effective prompt combines your editable persona with scoped notebooks and traits. Keys are `(chat, subject, layer, topic)`: in a group the subjects are `group` and `person:QQ_ID`; in private chat only the contact's `person:QQ_ID` is allowed. A person's records in two groups and in private chat are separate. Only the current group and most recent human speaker's notebooks enter generation. Shared group short-term context still includes other members' attributed messages; these must not be confused with the current speaker's traits. Retained candidate ideas are also partitioned by speaker to prevent personal context leaking through that cache.

| Layer | Contents and updates | Defaults per subject |
| --- | --- | --- |
| Long-term notebook | Topic-keyed summaries of past events, agreements and ongoing threads. Keep untouched topics, upsert an existing key to revise it, explicitly forget obsolete topics. | 1,800 characters, at most 24 topics, 365 days since update |
| Short-term details | Attributed original human messages, recorded automatically without a model call. Group and personal views deduplicate on retrieval. | 40 entries, 1,000 characters each, 72 hours |
| Traits | Stable interests, expression habits, interaction style and group themes. Updated separately from event history. | 900 characters, at most 24 topics, 180 days since update |

Long-term storage borrows the idea of input-dependent selection and recurrent state from [Mamba's selective state spaces](https://arxiv.org/abs/2312.00752), implemented here as a bounded application-level notebook, **not a Mamba neural network or mathematical reproduction of its SSM**. The model proposes up to four topic patches in `FORM`; unchanged topics persist. Under capacity pressure, higher-importance entries survive, with recently updated entries breaking ties. Source IDs, authors and timestamps remain inspectable; changed notebook/trait text retains a bounded revision history. Long-term and trait expiry is independent of raw chat-log retention; all state persists in SQLite. At most 200 people per chat are retained by default, evicting least recently updated personal scopes when full.

**Configuration → Adaptive persona & memory** controls these limits and learning cadence (default: 8 new human messages and 300 seconds). The same `FORM` call analyzes the current group/current speaker; it does not run a separate reflection for every past member. Personal updates must cite only that person's actual messages in the current chat. Group updates require messages from at least two distinct members. This validates attribution, not factual truth or consensus. The model is instructed to preserve disagreement and never infer group-wide preferences from one person. A wrong author, scope, layer, source or malformed patch rejects the update batch. No extra API request is required, though structured output uses tokens.

Recall uses the latest three human messages as a query. Notebook/trait context and short-term/owner-note recall are ranked separately with BM25-style term scoring (including Chinese character pairs), reciprocal-rank fusion of relevance/recency/importance/confidence, and a diversity penalty for similar text. Unrelated short-term entries are excluded; owner notes retain a preference. Notebook context can still include useful background without a keyword match. Scope filtering occurs before ranking: other groups, private chats, and other people's notebooks never enter the candidate set. Up to 6 details are returned, excluding current prompt-history messages. Each path has a default 2,400-character text budget; metadata is additional. No embedding API or vector database is required; paraphrases without shared words or supplied keywords may still be missed.

This refinement references [MaiBot's A_Memorix configuration](https://docs.mai-mai.org/manual/configuration/amemorix-config) and the [A_memorix sparse retrieval implementation](https://github.com/A-Dawn/A_memorix/blob/main/src/a_memorix/core/retrieval/sparse_bm25.py). It independently adapts evidence tracking and sparse multi-signal retrieval to the existing three layers. It does not install A_Memorix or reproduce its vector indexes, graph traversal, or separate Episode service. Long-term prompts encourage concise event summaries with participants, timing, progress and outstanding agreements; traits remain separate.

Updates may provide up to eight `keywords` and a subjective `confidence` (0–1). Older data receives confidence 0.6 and empty keywords during automatic migration. Low-confidence entries remain stored but are excluded from generation below `agent.memory.minConfidence` (default 0.35). Original short-term messages have confidence 1 for faithful recording, which does not prove their claims true. Confidence and relevance are not calibrated truth scores.

Changed text retains the previous three revisions by default; these are visible in the dashboard's memory details and never injected as current facts. Current records retain up to 12 attributed evidence references. Evidence older than a record's newest source cannot overwrite it. Repeating the same text/evidence does not change its revision or extend its lifetime; rewording without new evidence also does not refresh expiry. Explicit forgetting, expiry, capacity eviction and scope reset delete associated revisions. This is bounded audit history, not a backup or automatic semantic contradiction detector.

**Configuration → Adaptive persona & memory** additionally exposes recall character budget, recency half-life (default 30 days), minimum confidence and revision count (1–10). Recency changes ranking, not retention. These refinements add no separate model request; optional structured metadata consumes output tokens.

**Activity & logs → Learned chat styles & memories** shows up to 200 entries, prioritizing notebooks/traits, separated by chat, subject and layer. Reset clears that subject's three layers and learned expressions/jargon, retaining the original chat history and other subjects; in-flight learning is invalidated. New messages can teach it again. Disabling learning stops capture, updates and use of all three layers; current conversation context and owner notes remain available. Lowered limits are applied on live reload. Longer retention settings apply to future updates rather than resurrecting expired data.

Upgrade note: previous unscoped `learned_memories` and `chat_learning.style` are retained locally as legacy records but are not loaded into this new pipeline. They are not automatically assigned to a person or a group. The old `learning.maxMemories` and `learning.memoryDays` settings remain accepted for legacy compatibility; new layer limits use `agent.memory.*`. Sending probability, cooldowns, quiet hours and activity schedules continue to apply.

### Persona, expressions, jargon and emoji / 人格、表达、黑话与表情

Inspired by the separation of personality, behavior and reply style in [MaiBot's bot configuration](https://docs.mai-mai.org/manual/configuration/bot-config), the message pipeline now carries an explicit `personality` context through formation, evaluation, forecasting and articulation. Your existing `agent.persona` remains the identity and highest-priority owner preference; it is not overwritten. **Voice & purpose** adds `agent.personality.behavior`, `replyStyle`, `interests`, newline-separated `variants`, and `variantProbability` (default 0). At most one optional variant is selected per cycle. Style choices cannot override reply language, identity, schedules or sending controls. Chinese prompts prioritize the immediate conversational situation and one useful focus, with restrained humor during serious help or emotional support.

**Expressions, jargon & emoji** separates learning (`agent.expression.learn`) from use (`useLearned`), both on by default. The existing memory master switch and learning cadence also apply. The same `FORM` response can propose up to four expression/jargon records; no separate mining request is made. Each record includes its chat and subject, a term, meaning, applicable situation, literal example, confidence and evidence IDs. Jargon terms must appear in every cited message; expression examples must likewise be present. Personal evidence must come from that person. Before use, at least two distinct messages are required; group patterns additionally need two authors. Confidence must reach 0.8 by default. This checks attribution and repetition, not whether the model inferred the meaning correctly.

Recall filters to the current group/current speaker (or private contact) before lexical ranking. Unrelated patterns are not injected. Two candidates are supplied by default and the prompt asks to use at most one only when the situation fits. Entries expire after 90 days and are capped at 100 per chat. Confirmed literal reuse suppresses the entry for 30 minutes; paraphrased reuse is not detected. Repeated evidence does not refresh age, and changing a meaning requires fresh evidence and restarts evidence accumulation. **Activity & logs → Learned expressions & jargon** displays records, including candidates that may not yet meet use thresholds. Subject resets clear that subject's expressions and three memory layers together and invalidate in-flight learning.

Emoji are optional decorations, selected during the existing articulation request. By default, 15% of eligible cycles offer the configured text palette, with a 10-minute per-chat interval after confirmed use. The model can decline, especially in serious contexts. Configure `symbols` (Unicode emoji or kaomoji, one per line) and `faceIds` (allowed numeric QQ native face IDs; empty by default). Only an allowlisted choice becomes an appended symbol or a typed OneBot `face` segment. The application attaches at most one decoration in the same send action as the text, retains text escaping, and does not retry ambiguous delivery. Probability/cooldown govern this decoration selection; free-form model text is not a comprehensive emoji filter.

This is a lightweight adaptation, not MaiBot's full sticker collection/vision system: it does not download images, recognize sticker meanings, install MaiBot plugins, or add vector-model dependencies. Incoming native face IDs remain visible as markers; other attachments remain uninterpreted. Naturalness and inferred slang meanings still need observation in real chats.

### Request flow and capability limits

A cycle normally uses two API requests when candidate selection withholds, three when the sending forecast or probability check withholds, and four when sending. Disabling the sending policy removes the forecast request. Bursts are coalesced; up to two chats run concurrently. If new input arrives during generation, the stale response is discarded and the latest context is processed. Uncertain delivery outcomes are recorded and never automatically resent, favoring occasional missed delivery over duplicate posts.

Text, mentions and allowlisted native QQ face decorations are supported. Images, audio, files, and forwarded-message contents are represented by attachment markers, not downloaded or interpreted. The agent has no shell, browser, file-access, or other action tools. It can converse, not execute general-purpose tasks.

## Operation and local data

### Everyday commands

| Command | Purpose |
| --- | --- |
| `./agent setup` | Configure provider, key, selected chats, name, persona, and preview mode |
| `./agent contacts` | List available group and friend IDs locally |
| `./agent status` | Service state and recent connection status |
| `./agent logs` | Follow operational logs; no message bodies or API keys |
| `./agent stop` | Stop automatic participation |
| `./agent restart` | Reload configuration and restart |
| `./agent start` | Run in the foreground; refuses a second instance |
| `./agent check --api` | Authenticate and make one small model test request |
| `./agent test` | Run offline tests with local mock providers and OneBot |
| `./agent add-memory group:123 "The club meets on Sundays."` | Add an owner-authored note for exactly that chat |

Private memory uses `private:123` instead of `group:123`. Only selected chats are processed. To enable more chats, rerun setup or edit `config.json` and restart. Enter `-` to clear a list in setup. Removing a chat stops future processing; already stored local history remains until retention expiry.

The `qq-inner-agent.service` user service restarts after crashes. A WebSocket heartbeat checks QQ every 30 seconds; failed connections retry with exponential backoff and jitter. HTTP requests reuse connection pools, have deadlines, and retry transient failures. Conversation state is stored locally, rather than relying on a model server to remember a session. The program does not send paid keep-alive model requests.

`loginctl show-user "$USER" -p Linger` shows whether user services survive logout. Enable it with `loginctl enable-linger "$USER"` if needed. This does not make a desktop QQ session survive logout or suspend: keep the computer awake and QQ/SnowLuma available for uninterrupted service.

### Local files and credentials

- `config.json`: provider and behavior settings; no API key.
- `secrets.json`: API key and OneBot token, owner-only permissions. Do not share it.
- `data/agent.sqlite`: chat history, retained ideas, notes, decisions, budgets, delivery state. Default history retention is 30 days with up to 500 messages per chat. Notes remain until explicitly removed.
- `data/status.json`: latest operational status, updated every five seconds. Check `updatedAt` if the service is stopped.
- `src/`: application source; `test/`: offline tests; `scripts/`: setup and service installation.

Keys may instead be supplied via `LLM_API_KEY`, `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and `ONEBOT_TOKEN`. Shell environment variables are not automatically inherited by an already-running systemd user service; the local wizard is the simplest setup route.

## Troubleshooting and verification

- **`waiting_for_setup`**: run `./agent setup`; a key and at least one chat ID are needed.
- **QQ disconnected/offline**: sign in through NapCat or load QQ's process in SnowLuma, and enable its forward WebSocket server. Check the URL and token; the agent retries automatically. Use `./agent check` to verify the OneBot endpoint.
- **`http_401_check_provider_config` / `http_403_check_provider_config`**: check the key and provider account. Authentication/configuration errors back off for five minutes.
- **`http_400_check_provider_config` / `http_404_check_provider_config`**: check the base URL, model and `tokenParameter`.
- **`output_truncated_increase_maxTokens`**: increase `provider.maxTokens`; use a model/configuration that can finish structured output within that budget.
- **`invalid_json` / `invalid_ratings`**: the model returned invalid structured output; no message is sent. Use a model that follows JSON instructions.
- **`hourly_api_budget`**: the configured rolling limit has been reached. It recovers as calls age out of the window.
- **No unsolicited reply**: low motivation, cooldown, quiet hours, or conversation inactivity may correctly produce silence. See `decisions` in SQLite; silence is part of the design.
- **Service exits with 75**: another instance holds `.agent.lock`. Stop the foreground process before starting the service.

Run `./agent test` for local verification. Tests use fictitious conversations and local mock endpoints, not real QQ recipients or paid model calls. Live DeepSeek authentication can only be verified after you supply your key.

See [VERIFICATION.md](VERIFICATION.md) for test coverage and the limits of live verification.

## Research background

This is an implementation inspired by [Liu et al., *Proactive Conversational Agents with Inner Thoughts*, CHI 2025](https://arxiv.org/html/2501.00383v2), not a reproduction of its experiments or a claim to expose a model's hidden reasoning.

| Paper stage | This program |
| --- | --- |
| Trigger | New messages and one pause after recent human activity |
| Retrieval | Chat-scoped historical utterances and owner notes, ranked by lexical similarity and recency |
| Thought formation | A model generates brief candidate contributions, including fast acknowledgments and more deliberate ideas |
| Evaluation | A separate request rates motivation, relevance, originality, and supporting/withholding criteria |
| Participation | Turn allocation and thresholds determine whether to articulate a selected idea or retain it |

The candidate pool survives restarts and is reevaluated against new context. Open turns use a motivation threshold; explicit invitations receive priority; interrupting another person's turn requires a higher score. Configurable System 1 probability and tone control participation style.

Deliberate deviations: retrieval uses lexical similarity rather than embeddings; evaluation uses validated numerical model ratings rather than token-logprob-weighted scores. The silence multiplier is capped. Extra operational controls bound repetition, stale output, API usage, and activity during quiet hours. These scores are model judgments, not calibrated probabilities or evidence of subjective feelings.
