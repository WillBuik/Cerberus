use std::{collections::HashMap, sync::Arc};

use serde::{Serialize, Deserialize};
use tokio::sync::mpsc;
use tokio_util::sync::{DropGuard, CancellationToken};

/// Target for notifications.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum NotificationTarget {
    /// Send notifications to a Discord webhook.
    DiscordWebhook {
        /// Discord webhook URL.
        url: String,

        /// Optional username to override the webhook's default.
        username: Option<String>,
    }
}

/// Notification manager, handles sending status updates and alarms
/// to the configured notification targets.
#[derive(Clone)]
pub struct NotificationManager {
    /// Status notification channel sender.
    status_sender: mpsc::UnboundedSender<String>,

    /// Alarm notification channel sender.
    alarm_sender: mpsc::UnboundedSender<String>,

    /// Drop guard to shut down the notification manager's background
    /// task once the last handle to the manager is dropped.
    /// 
    /// Note, this is never read, by design.
    #[allow(dead_code)]
    cancelation_dropguard: Arc<DropGuard>,
}

impl NotificationManager {
    /// Create a new NotificationManager.
    pub fn new(status_target: Option<NotificationTarget>, alarm_target: Option<NotificationTarget>) -> Self {
        let shutdown_token = CancellationToken::new();
        let cancelation_dropguard = shutdown_token.clone().drop_guard();

        let (status_sender, status_receiver) = mpsc::unbounded_channel();
        let (alarm_sender, alarm_receiver) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            if let Err(err) = Self::background_task(status_target, alarm_target, status_receiver, alarm_receiver, shutdown_token).await {
                log::error!("Notification manager background task failed: {}", err);
            } else {
                log::info!("Notification manager background task finished");
            }
        });

        Self {
            status_sender,
            alarm_sender,
            cancelation_dropguard: Arc::new(cancelation_dropguard),
        }
    }

    /// Notification manager background task.
    async fn background_task(
        status_target: Option<NotificationTarget>,
        alarm_target: Option<NotificationTarget>,
        mut status_receiver: mpsc::UnboundedReceiver<String>,
        mut alarm_receiver: mpsc::UnboundedReceiver<String>,
        shutdown_token: CancellationToken)
     -> anyhow::Result<()>
    {
        loop {
            tokio::select! {
                Some(status) = status_receiver.recv() => {
                    if let Some(status_target) = &status_target {
                        if let Err(err) = send_notification(status_target, &status).await {
                            log::error!("Failed to send status notification '{}': {}", status, err);
                        }
                    }
                },

                Some(alarm) = alarm_receiver.recv() => {
                    if let Some(alarm_target) = &alarm_target {
                        if let Err(err) = send_notification(alarm_target, &alarm).await {
                            log::error!("Failed to send alarm notification '{}': {}", alarm, err);
                        }
                    }
                },

                _ = shutdown_token.cancelled() => {
                    break;
                }
            }
        }

        // Clean up and attempt to send remaining messages.
        status_receiver.close();
        alarm_receiver.close();
        // todo

        Ok(())
    }

    /// Send a status message to the status notification target.
    pub fn send_status<T: ToString> (&self, message: T) {
        if let Err(err) = self.status_sender.send(message.to_string()) {
            log::error!("Failed to send status message '{}', notification manager is stopped", err.0);
        }
    }

    /// Send an alarm message to the alarm and status notification targets.
    pub fn send_alarm<T: ToString> (&self, message: T) {
        if let Err(err) = self.alarm_sender.send(message.to_string()) {
            log::error!("Failed to send alarm message '{}', notification manager is stopped", err.0);
        }
    }
}

/// Send a notification to a target.
async fn send_notification(target: &NotificationTarget, message: &str) -> anyhow::Result<()> {
    match target {
        NotificationTarget::DiscordWebhook { url, username } => {
            let mut params = HashMap::new();
            params.insert("content", message);
            if let Some(username) = username {
                params.insert("username", username);
            }
            let client = reqwest::Client::new();
            let resp = client.post(url).form(&params).send().await?;
            resp.error_for_status()?;
        },
    }

    Ok(())
}