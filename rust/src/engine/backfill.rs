use super::*;

impl Engine {
    /// One bounded pass; individual group/message failures must not block other groups.
    pub async fn backfill_once(self: &Arc<Self>) {
        let settings = &self.config.agent.backfill;
        if !settings.enabled {
            return;
        }
        let mut abort = self.aborted.subscribe();
        for group in &self.config.agent.allowed_groups {
            if *abort.borrow() || !self.transport.state().connected {
                return;
            }
            let result = tokio::select! {
                biased;
                _ = abort.changed() => return,
                result = self.transport.call(
                    "get_group_msg_history",
                    json!({"group_id":group,"count":settings.count}),
                ) => result,
            };
            let messages = match result {
                Ok(data) => match data["messages"].as_array() {
                    Some(messages) => messages.clone(),
                    None => {
                        (self.options.log)(
                            "backfill_failed",
                            json!({"group":group,"code":"invalid_history"}),
                        );
                        continue;
                    }
                },
                Err(_) => {
                    (self.options.log)(
                        "backfill_failed",
                        json!({"group":group,"code":"history_request_failed"}),
                    );
                    continue;
                }
            };
            for mut event in messages {
                if *abort.borrow() || !self.transport.state().connected {
                    return;
                }
                if !event.is_object() {
                    continue;
                }
                event["post_type"] = json!("message");
                event["message_type"] = json!("group");
                event["group_id"] = json!(group);
                event["self_id"] = json!(self.transport.self_id());
                let engine = self.clone();
                // Same blocking I/O boundary as live ingest; always join before shutdown/reload.
                if !matches!(
                    tokio::task::spawn_blocking(move || {
                        if *engine.aborted.borrow() {
                            return Ok(());
                        }
                        engine.ingest_backfill(&event)
                    })
                    .await,
                    Ok(Ok(()))
                ) {
                    (self.options.log)(
                        "backfill_failed",
                        json!({"group":group,"code":"message_rejected"}),
                    );
                }
            }
        }
    }
}
