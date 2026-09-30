# QQ Inner Agent

A persistent conversational agent for one QQ account, connected through SnowLuma's OneBot v11 WebSocket. Each enabled group and private contact has its own context, memory, and pool of candidate contributions. DeepSeek is preconfigured; OpenAI-compatible Chat Completions and Anthropic-compatible Messages endpoints are supported.

## Finish setup

In a terminal:

```bash
cd qq-inner-agent
./agent contacts       # list group/contact IDs through your own QQ account
./agent setup          # enter the API key without terminal echo; choose chat IDs
./agent check --api    # verify QQ and a small model request; sends no QQ message
./agent status
```

Install the background service with `./agent install-service`. Setup restarts an installed service after saving changes. Before a key and chat IDs are entered, the running agent keeps trying to connect to SnowLuma but does not invoke the model or send QQ messages. Selecting a chat enables its new messages to be processed by the configured model provider. No old chat history is fetched from QQ.

For another machine, install Node.js 22.13+ (or set `AGENT_NODE` to its executable), connect SnowLuma, configure `onebot.url`, `onebot.selfId`, and `secrets.json.onebotToken`, then run `./agent install-service`. The bundled `.runtime/node` on this machine is a separate copy without SnowLuma's tracing capability; this agent needs no elevated permissions.

QQ must be logged in, SnowLuma's hook must be connected, and the OneBot WebSocket must be running. The agent reconnects when these recover; it cannot log QQ in or repair the hook itself.

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

A cycle normally uses two API requests when withholding, and three when sending. Bursts are coalesced; up to two chats run concurrently. If new input arrives during generation, the stale response is discarded and the latest context is processed. Uncertain delivery outcomes are recorded and never automatically resent, favoring occasional missed delivery over duplicate posts.

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
- **QQ disconnected/offline**: open QQ, sign in, and load its process in SnowLuma; the agent retries automatically. Use `./agent check` to verify the OneBot endpoint.
- **`http_401_check_provider_config` / `http_403_check_provider_config`**: check the key and provider account. Authentication/configuration errors back off for five minutes.
- **`http_400_check_provider_config` / `http_404_check_provider_config`**: check the base URL, model and `tokenParameter`.
- **`output_truncated_increase_maxTokens`**: increase `provider.maxTokens`; use a model/configuration that can finish structured output within that budget.
- **`invalid_json` / `invalid_ratings`**: the model returned invalid structured output; no message is sent. Use a model that follows JSON instructions.
- **`hourly_api_budget`**: the configured rolling limit has been reached. It recovers as calls age out of the window.
- **No unsolicited reply**: low motivation, cooldown, quiet hours, or conversation inactivity may correctly produce silence. See `decisions` in SQLite; silence is part of the design.
- **Service exits with 75**: another instance holds `.agent.lock`. Stop the foreground process before starting the service.

Run `./agent test` for local verification. Tests use fictitious conversations and local mock endpoints, not real QQ recipients or paid model calls. Live DeepSeek authentication can only be verified after you supply your key.


## Remote web dashboard

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
