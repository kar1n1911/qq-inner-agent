# QQ Inner Agent

A persistent conversational agent for one QQ account, connected through NapCat or SnowLuma's forward OneBot v11 WebSocket. Each enabled group and private contact has its own context, memory, and pool of candidate contributions. DeepSeek is preconfigured; OpenAI-compatible Chat Completions and Anthropic-compatible Messages endpoints are supported.

## Sending probability and expectations / 发送策略与预测

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

## Adaptive persona and simple memory RAG / 自适应角色与记忆

The default Chinese persona now emphasizes picking up conversational threads, light associations and humor, and starting relevant, easy-to-answer topics without constant questions or forced activity. Exact previous default personas migrate automatically; custom personas remain unchanged.

The effective prompt context combines your editable base persona with a separate learned style for each chat. The model can rewrite that style as it analyzes new messages, but cannot rewrite system rules, configuration or another chat's profile. **Configuration → Adaptive persona & memory** controls learning. By default it requires at least 8 new human messages and a 300-second interval. Learning is an optional structured part of `FORM`, so it adds no separate API call (the response can use more tokens). Updates require real human message IDs from the supplied context; the application attaches source authors and timestamps. Unsupported or malformed updates are discarded. This validates provenance, not the factual accuracy of every model summary.

Each learning update can retain up to 3 short memories (preferences, agreements, unfinished topics), replace the style summary, and remove up to 3 retrieved outdated memories. Default storage is 100 learned memories per chat, expiring after 30 days or the shorter global retention period. Learned data persists across restarts in SQLite.

Simple local RAG uses the latest three human messages as a query against up to 500 older chat messages, up to 500 non-expired learned memories and 50 owner notes in that same chat. Ranking uses lexical overlap including Chinese character pairs and recency, with a small owner-note preference. The top 6 entries by default are supplied to formation, evaluation, forecast and articulation with source/time metadata. Current prompt history is excluded from raw-history retrieval. No embedding API or vector database is required; paraphrases without keyword overlap may be missed.

**Activity & logs → Learned chat styles & memories** shows recent profiles, memories and sources, with a per-chat reset. Reset retains original chat history and blocks in-flight work from restoring the erased learning; subsequent new messages may teach the agent again. Disabling learning stops both learning updates and the use of learned style/memories; existing raw-history/owner-note retrieval continues. Nothing in learned context overrides sending probability, cooldowns, quiet hours or explicit activity schedules.

## Finish setup

In a terminal:

```bash
cd qq-inner-agent
./agent contacts       # list group/contact IDs through your own QQ account
./agent setup          # enter the API key without terminal echo; choose chat IDs
./agent check --api    # verify QQ and a small model request; sends no QQ message
./agent status
```

Install the background service with `./agent install-service`. Setup restarts an installed service after saving changes. Before a key and chat IDs are entered, the running agent keeps trying to connect to the QQ bridge but does not invoke the model or send QQ messages. Selecting a chat enables its new messages to be processed by the configured model provider. No old chat history is fetched from QQ.

For another machine, install Node.js 22.13+ (or set `AGENT_NODE` to its executable), connect NapCat or SnowLuma, configure `onebot.url`, `onebot.selfId`, and `secrets.json.onebotToken` with `./agent setup`, then run `./agent install-service`. This agent needs no elevated permissions.

QQ must be logged in through the chosen bridge, and its OneBot WebSocket server must be running. The agent reconnects when these recover; it cannot log QQ in or repair the bridge itself.

## Connect NapCat

1. Start NapCat and sign in to QQ. In NapCat WebUI, open **Network configuration → New → WebSocket server** (正向 WebSocket). The agent connects as a client; reverse WebSocket and HTTP-only endpoints are not supported.
2. Enable the server on port `3001`, with **message format `array`**, a nonempty access token, and host `127.0.0.1` when both programs run directly on the same host. Keep event pushing enabled. The equivalent server entry is in [examples/napcat-websocket-server.json](examples/napcat-websocket-server.json); add it to NapCat's `network.websocketServers` list, replacing the token placeholder. It is a single server entry, not a complete NapCat configuration.
3. Run `./agent setup` and enter `ws://127.0.0.1:3001/`, the same OneBot token, and optionally the QQ account ID. A blank account ID in the dashboard (or `-` in the wizard) detects the logged-in account. Alternatively, edit these under **Configuration → NapCat / SnowLuma** in the dashboard. Save before loading contacts.
4. Run `./agent check` to verify the bridge and `./agent contacts` to list available IDs. Select chats and enter the model API key before starting participation.

For NapCat in Docker with the agent on the host, bind the WebSocket server to `0.0.0.0` **inside the container** and publish `127.0.0.1:3001:3001`. The agent still connects to `ws://127.0.0.1:3001/`. For another machine, use a TLS WebSocket proxy (`wss://`) or an SSH tunnel to a local port. Plain `ws://` is accepted only for localhost. Port `3000` is normally HTTP, and NapCat's WebUI port is not the OneBot WebSocket endpoint.

Use the root WebSocket path `/`, which carries both actions and events; NapCat's `/api` path does not deliver events. Both array and CQ-string message events are accepted; outgoing messages always use text segments. Self-message reports are ignored. NapCat and SnowLuma need separate ports if running together; configure the agent for one bridge at a time.

Compatibility is based on NapCat's [network documentation](https://doc.napneko.icu/onebot/network), [configuration schema](https://github.com/NapNeko/NapCatQQ/blob/main/packages/napcat-onebot/config/config.ts), and [WebSocket implementation](https://github.com/NapNeko/NapCatQQ/blob/main/packages/napcat-onebot/network/websocket-server.ts). Automated mock tests cover authentication, login/status, contacts, mentions, private replies, self-message filtering, and both event formats. A live NapCat account has not yet been used for end-to-end verification.

## Everyday commands

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

## DeepSeek and other providers

The wizard offers `deepseek-openai`, `deepseek-anthropic`, and `custom`. The current DeepSeek preset uses `deepseek-flash`; you can enter another model available to your API account. The two DeepSeek profiles use the same API key.

| Profile | API base URL | Request format |
| --- | --- | --- |
| DeepSeek OpenAI | `https://api.deepseek.com` | `POST /chat/completions`, bearer authentication |
| DeepSeek Anthropic | `https://api.deepseek.com/anthropic` | `POST /v1/messages`, `x-api-key` and `anthropic-version` |
| OpenAI-compatible | Your base URL, typically ending in `/v1` | `POST /chat/completions` relative to the base |
| Anthropic-compatible | Your base URL | Appends `/v1/messages`, or `/messages` when base ends in `/v1` |

A full endpoint ending in `/chat/completions` or `/messages` is also accepted. `provider.tokenParameter` selects `max_tokens` for DeepSeek/older gateways or `max_completion_tokens` for OpenAI. `provider.anthropicAuth` supports `x-api-key` or `bearer`; `workspaceId` is available for Anthropic keys that require it. The DeepSeek presets disable extended thinking to keep the short structured stages responsive. The application reads only final text, not provider reasoning fields.

The adapters follow the [OpenAI Chat Completions reference](https://developers.openai.com/api/reference/python/resources/chat/subresources/completions/methods/create), [Anthropic Messages guide](https://platform.claude.com/docs/en/build-with-claude/working-with-messages), and [DeepSeek API documentation](https://api-docs.deepseek.com/). Model availability and gateway parameter support depend on the provider.

## How the paper informs behavior

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

## Participation settings

### 语言设置 / Language

控制台默认使用简体中文。页面顶部和“配置 → 语言设置”可切换 **简体中文 / English**；登录前的选择保存在当前浏览器，登录后使用已保存的界面语言。登录后点击“保存并应用”可保存界面和回复语言。回复语言可选 **跟随聊天语言、简体中文、English**，与界面语言相互独立。

候选生成、评分和最终发言的系统提示词均使用中文；切换英文界面不会把提示词改成英文。JSON 字段名、协议标识、模型 ID 和原始日志保持不变。默认角色说明也改为中文，升级时仅替换原版英文默认角色说明，自定义角色设定保持原样。

The dashboard defaults to Simplified Chinese, with an English option. Save interface and reply-language preferences under Configuration → Language settings. System prompts remain Chinese for either interface language. Replies can follow the conversation or use a fixed Chinese/English language. Existing custom personas and conversation content are not translated.

In **Configuration → Active and inactive times**, enable the daily schedule, choose **Active from**, **Inactive from**, and an IANA time zone (for example `Europe/Stockholm`). The active interval includes its start and excludes its end. Overnight windows such as 22:00–06:00 work too. Disable the schedule for 24-hour availability; equal start/end times are rejected. While inactive, the agent skips incoming messages and pauses all automatic replies, including mentions and private messages. Queued work is discarded at the cutoff, and an in-flight response cannot be sent after inactive hours begin. Connections and manual diagnostics remain available. Quiet hours are a separate restriction on proactive replies within active hours.

In **Choose a model**, click **Load available models** after saving your provider URL, API format, and key. Select a returned ID or enter one manually in **Model & API**, then **Save & apply**. Listing models does not generate a completion. The list uses the provider's first returned page (up to 500 IDs); gateways without a model-list API and models omitted from that page can use manual entry. DeepSeek uses its [models endpoint](https://api-docs.deepseek.com/api/list-models/) for both API formats; Anthropic-compatible providers use their [Messages API model-list route](https://platform.claude.com/docs/en/api/models).

Use the dashboard to save and apply settings live, or edit `config.json` locally. The agent checks for changes every second; invalid edits leave the previous configuration running. `config.example.json` shows all defaults.

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

A cycle normally uses two API requests when candidate selection withholds, three when the sending forecast or probability check withholds, and four when sending. Disabling the sending policy removes the forecast request. Bursts are coalesced; up to two chats run concurrently. If new input arrives during generation, the stale response is discarded and the latest context is processed. Uncertain delivery outcomes are recorded and never automatically resent, favoring occasional missed delivery over duplicate posts.

Text and mentions are supported. Images, audio, files, and forwarded-message contents are represented by attachment markers, not downloaded or interpreted. The agent has no shell, browser, file-access, or other action tools. It can converse, not execute general-purpose tasks.

## Local files

- `config.json`: provider and behavior settings; no API key.
- `secrets.json`: API key and OneBot token, owner-only permissions. Do not share it.
- `data/agent.sqlite`: chat history, retained ideas, notes, decisions, budgets, delivery state. Default history retention is 30 days with up to 500 messages per chat. Notes remain until explicitly removed.
- `data/status.json`: latest operational status, updated every five seconds. Check `updatedAt` if the service is stopped.
- `src/`: application source; `test/`: offline tests; `scripts/`: setup and service installation.

Keys may instead be supplied via `LLM_API_KEY`, `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and `ONEBOT_TOKEN`. Shell environment variables are not automatically inherited by an already-running systemd user service; the local wizard is the simplest setup route.

## Troubleshooting

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


## Remote web dashboard

The Overview page includes two QQ diagnostics, independent of the model and chat allowlists:

- **Send self-test message** sends one uniquely marked text message to the QQ account returned by the bridge's login API. A successful result means the bridge accepted the action; check QQ's self-chat to confirm delivery. The recipient and message cannot be overridden. Failed or uncertain sends are not retried automatically.
- **Start receiving test** listens for 60 seconds. From that same QQ account, send several messages (text, image, voice, file, reply) to yourself or another chat. Enable **reportSelfMessage** on the bridge's WebSocket server first; some bridges do not emit self-chat events even when sending succeeds. The viewer accepts `message` and `message_sent` events only when both the account and sender match the logged-in account. It displays the last 30 events, text previews and segment types; it does not fetch attachments, invoke the AI, or write captures to disk. **Stop receiving** closes the diagnostic connection early. Starting a new test clears the previous capture.

If receiving stays empty, check self-message reporting and that the URL carries events (use `/`, not `/api`). An empty capture is not a successful receiving test. Diagnostics use the saved connection settings, so save URL/token changes first. They can run while the agent service is stopped.

The installed dashboard runs independently of the agent:

- Local host: http://localhost:5097
- LAN: `https://<host-LAN-IP>:5098`
- VPN: `https://<host-VPN-IP>:5098`

On a remote device, import the public `dashboard-ca.crt` into its trusted certificate authorities before opening the HTTPS address. Keep `data/dashboard-ca.key` and all other private keys on this host. The certificate covers the addresses available during installation; run `./agent install-dashboard` again if addresses change. Remote connectivity still depends on your LAN/VPN routing and firewall. The HTTP listener is loopback-only.

Retrieve your login key on the host with `./agent dashboard-key`. Sessions expire after 12 hours. HTTPS, HttpOnly cookies, CSRF checks, an origin allowlist, and login rate limiting protect remote access. Do not expose this private administration service to the public internet. The dashboard key gives access to configuration, retained conversation ideas, operational logs, and agent controls.

In **Configuration**, enter your DeepSeek key, choose the API format/model, and enable the desired group/private IDs. **Load QQ contacts** can populate the selection from the signed-in QQ account. API keys remain blank after saving; the interface only indicates whether a key is configured. Choose **Save & apply** to reload the agent. In-flight generation is cancelled when its configuration becomes obsolete; stored memory remains. The application reports when the saved revision has been applied. A provider-host change requires a replacement key or explicit clearing of the old key.

**Overview** shows service, QQ and model status, readiness, and recent decisions. **Activity & logs** shows retained candidate contributions and refreshes operational logs every two seconds. **Test API connection** makes one small request using the saved provider key and counts against the configured API budget; it sends no QQ message. Start, stop, and restart control only the agent service, leaving the dashboard available.

Commands:

```sh
./agent install-dashboard   # Generate certificates and install/refresh the user service
./agent dashboard-key       # Display the private login key locally
./agent dashboard           # Foreground dashboard (stop its service first)
systemctl --user status qq-inner-dashboard.service
systemctl --user restart qq-inner-dashboard.service
```

`dashboard.json` contains bind addresses, permitted origins, and TLS paths. Dashboard-server changes require a dashboard restart. Agent behavior settings reload live. Secrets/configuration saves use a recovery journal so an interrupted two-file write can roll back when the dashboard restarts.
