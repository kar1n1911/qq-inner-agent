# Verification — 2026-10-01

- 61 tests passed, 0 failures (`./agent test`).
- Three-layer memory tests cover QQ-ID attribution, personal/group/private isolation, multi-author group evidence, rejection of wrong/self/cross-chat sources, short-term deduplication and expiry, selective notebook revision/forget/capacity, independent long-term retention, per-subject reset, restart persistence, live limits, and candidate-cache isolation between speakers. Notebook behavior is application-level state management, not a trained Mamba model. No live model or QQ calls were used for these tests.
- Adaptive-persona tests cover custom-persona preservation, configuration bounds, human-source validation, applying learned style/RAG in articulation without an extra API call, learning cadence/disable behavior, Chinese retrieval, context exclusion, isolation, memory limits/expiry, persistence and reset during generation. Dashboard tests verify authenticated/CSRF-protected inspection and reset. Browser QA showed Chinese learning controls and a clearly labeled synthetic profile in an isolated preview. Social effectiveness with a live model remains unverified.
- Sending-policy tests cover time/pace factors, probability bounds, forecast validation and vetoes, single-attempt persistence across pauses/restarts, stale forecasts, articulation plans, expiring chat-isolated expectations, and dry-run/uncertain-delivery behavior. Both API formats exercise the four-stage pipeline against local HTTP and WebSocket mocks. Browser QA verified the Chinese controls and saving a probability in an isolated dashboard. No live model or QQ requests were made for this feature.
- Language tests validate independent interface/reply settings, Chinese prompt contracts, translated UI messages, and selected reply-language instructions in the actual engine pipeline. Browser QA uses an isolated configuration to check Chinese/English switching and saving, without live model calls or QQ messages.
- Activity schedule tests cover exact minute boundaries, time zones, overnight intervals, invalid schedules, direct-request suppression, queued-work cutoff, and a response crossing into inactive hours. Model-list tests verify provider URL/authentication selection, deduplication, and failure handling without inference.
- QQ dashboard diagnostic tests cover fixed self-account sending, capture of multiple self-message formats, filtering other accounts/senders, credential redaction, bounded capture, timeout/stop cleanup, and authenticated/CSRF-protected routes. Real self-chat delivery and bridge self-event reporting still require a live QQ bridge.
- NapCat-shaped forward WebSocket mock tests cover array and CQ-string events, group mentions, private replies, contact lists, lifecycle/heartbeat events, self-message filtering, and rejection of invalid tokens after upgrade. Live NapCat login and message delivery remain unverified.
- Dashboard tests cover authenticated access, origin and CSRF checks, secret redaction, configuration validation, stale revisions, and interrupted-save recovery.
- A process-level test verifies live configuration reload, rejection of invalid edits, and recovery without changing the agent PID.
- Both model API adapters exercised against real local HTTP mock endpoints.
- OneBot transport exercised against a local WebSocket mock: authentication, request correlation, sending text segments, heartbeat timeout, reconnection, and duplicate-event suppression.
- Decision tests cover turn allocation, interruption threshold, withholding, retained ideas, quiet hours, cooldowns, chat isolation, stale responses, dry-run mode, invalid model output, API budgets, and ambiguous delivery outcomes.
- A regression test verifies that the DeepSeek profile never uses an unrelated OpenAI or Anthropic environment key.
- Live SnowLuma OneBot authentication succeeded; the test account reported online.
- The installed user service reported `onebotConnected: true`, `qqOnline: true`, and `mode: waiting_for_setup` during verification. SnowLuma and the dashboard were subsequently stopped at the owner's request.
- User-service lingering is enabled. The service can survive logout; the GUI QQ session and host still need to remain available.
- Single-instance protection rejected a second foreground start with exit code 75.
- Local configuration and secrets have mode `0600`; the data directory has mode `0700`.
- The agent's separate Node runtime has no Linux file capabilities.
- Python setup scripts and shell launcher passed syntax checks. The generated service unit loaded successfully after correcting WorkingDirectory formatting.

Not yet verified: a live DeepSeek completion or an actual QQ reply from this program. No DeepSeek key or selected chat IDs have been provided. No real QQ messages were sent during verification, and no paid model calls were made. Use `./agent setup`, followed by `./agent check --api`, to complete local configuration and verify provider access.
