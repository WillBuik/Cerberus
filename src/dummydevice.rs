use std::time::Duration;

use async_trait::async_trait;

use crate::{DeviceMonitor, DeviceId, status::{StatusManager, StatusLevel}, backgroundtask::BackgroundTask};

/// Dummy device monitor for testing.
pub struct DummyDeviceMonitor {
    id: DeviceId,
    task: BackgroundTask<()>,
}

impl DummyDeviceMonitor {
    pub fn new(status_manger: StatusManager, states: Vec<(String, bool)>, period: u64) -> anyhow::Result<Self> {
        if states.len() == 0 {
            anyhow::bail!("dummy device must have at least one state");
        }

        let id = DeviceId::default();

        let task = BackgroundTask::spawn(|shutdown_token| {
            async move {
                status_manger.update_status(id, "Dummy device monitor started.", StatusLevel::Info).await;

                let mut current_state = 0;

                loop {
                    let (state_message, is_alarm) = &states[current_state];
                    if *is_alarm {
                        status_manger.update_status(id, state_message, StatusLevel::Alarm).await;
                    } else {
                        status_manger.update_status(id, state_message, StatusLevel::Status).await;
                    }

                    let next_state = tokio::time::sleep(Duration::from_secs(period));

                    tokio::select! {
                        _ = next_state => {
                            current_state += 1;
                            if current_state >= states.len() {
                                current_state = 0;
                            }
                        }
                        _ = shutdown_token.cancelled() => {
                            break;
                        }
                    }
                }

                status_manger.update_status(id, "Dummy device monitor stopped.", StatusLevel::Info).await;
            }
        });

        Ok(Self {
            id,
            task
        })
    }
}

#[async_trait]
impl DeviceMonitor for DummyDeviceMonitor {
    async fn shutdown(&mut self) {
        let _ = self.task.finish().await;
    }

    fn id(&self) -> DeviceId {
        self.id
    }
}
