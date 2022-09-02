use std::{fmt::Display, collections::HashMap, sync::Arc, convert::Infallible, net::SocketAddr};

use tokio::sync::{RwLock, Mutex};
use warp::Filter;

use crate::{notification::NotificationManager, DeviceMonitor, DeviceId, backgroundtask::BackgroundTask};

/// Status severity levels for device monitor updates and logging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusLevel {
    /// Low-priority info about a device, not sent to any notification target.
    Info,
    /// Device monitor non-alarm status update, sent to status notification targets.
    Status,
    /// High-priority info about a device, sent to alarm notification targets.
    Warning,
    /// Device monitor alarm status update, sent to alarm notification targets.
    Alarm,
}

struct StatusEntry {
    message: String,
    timestamp: u64,
    level: StatusLevel
}

/// Internal status manager data.
#[derive(Default)]
struct StatusData {
    /// Device registration.
    devices: Vec<(DeviceId, String)>,

    /// Status storage.
    statuses: HashMap<DeviceId, Vec<StatusEntry>>
}

/// Status manager handle, allows device monitors to update status and
/// serves status updates via HTTP.
#[derive(Clone)]
pub struct StatusManager {
    notification_manager: NotificationManager,
    log_device_id: DeviceId,
    status_data: Arc<RwLock<StatusData>>,
    server_task: Arc<Mutex<Option<BackgroundTask<()>>>>,
}

impl StatusManager {
    /// Create a new status manager.
    pub fn new(notification_manager: NotificationManager) -> Self {
        let manager = Self {
            notification_manager,
            log_device_id: Default::default(),
            status_data: Default::default(),
            server_task: Default::default(),
        };
        
        // Register log device.
        {
            let mut status_data = manager.status_data.try_write().expect("status data must be unlocked");
            status_data.devices.push((manager.log_device_id, "Log".to_string()));
        }

        manager
    }

    /// Register a device monitor with the status manager.
    pub async fn register_device(&self, device_monitor: &dyn DeviceMonitor) {
        let mut status_data = self.status_data.write().await;
        status_data.devices.push((device_monitor.id(), "Device".to_string()))
    }

    /// Submit a status update for a device.
    pub async fn update_status<T: ToString + Display> (&self, device_id: DeviceId, message: T, level: StatusLevel) {
        let status_entry = StatusEntry {
            message: message.to_string(),
            timestamp: 0,
            level,
        };

        let mut status_data = self.status_data.write().await;

        // Send status to application log.
        let mut device_name = "Unknwon Device".to_string();
        for (i_id, i_device_name) in &status_data.devices {
            if device_id == *i_id {
                device_name = i_device_name.clone();
            }
        }
        let log_message = format!("[{}, {:?}] {}", device_name, level, message);
        match level {
            StatusLevel::Info | StatusLevel::Status => log::info!("{}", log_message),
            StatusLevel::Warning => log::warn!("{}", log_message),
            StatusLevel::Alarm => log::warn!("{}", log_message),
        }

        // Add to status list.
        if let Some(device_statuses) = status_data.statuses.get_mut(&device_id) {
            device_statuses.push(status_entry);
        } else {
            status_data.statuses.insert(device_id, vec![status_entry]);
        }

        // Send notifications.
        match level {
            StatusLevel::Info | StatusLevel::Status => self.notification_manager.send_status(log_message),
            StatusLevel::Warning | StatusLevel::Alarm => self.notification_manager.send_alarm(log_message),
        }
    }

    /// Submit a log message to the status manager.
    /// 
    /// The log message will be forwarded to the configured application
    /// logger and sent to the appropriate notification targets.
    pub async fn log<T: Display> (&self, message: T, level: StatusLevel) {
        match level {
            StatusLevel::Info => log::info!("{}", message),
            StatusLevel::Status => log::info!("{}", message),
            StatusLevel::Warning => log::warn!("{}", message),
            StatusLevel::Alarm => log::error!("{}", message),
        }
        self.update_status(self.log_device_id, format!("{}", message), level).await;
    }

    /// Start the status HTTP server on a background thread.
    pub async fn serve(&self) -> anyhow::Result<()> {
        let mut server_task = self.server_task.lock().await;
        if let Some(_) = *server_task {
            anyhow::bail!("status server already started");
        }

        // Warp, why, this is horrible :(
        let self_inner1 = self.clone();
        let test = warp::path!("status_txt").and_then(move || {
            let self_inner2 = self_inner1.clone();
            async move {
                self_inner2.status_txt().await
            }
        });

        // Start warp server in a background task.
        let task_result = BackgroundTask::try_spawn(|shutdown_token| {
            // Wrap the shutdown token in a future for bind_with_graceful_shutdown.
            let shutdown_future = async move {
                shutdown_token.cancelled().await;
            };

            let bind_result = warp::serve(test)
                .try_bind_with_graceful_shutdown("[::]:8080".parse::<SocketAddr>().unwrap(), shutdown_future);

            // If we were able to bind to the port, start the server.
            match bind_result {
                Ok(server) => Ok(async move { server.1.await } ),
                Err(err) => Err(anyhow::anyhow!(err)),
            }
        });

        *server_task = Some(task_result?);

        Ok(())
    }

    async fn status_txt(&self) -> Result<String, Infallible> {
        let mut status_text = String::new();
        let status_data = self.status_data.read().await;

        status_text.push_str("Cerberus Status:\n");

        for (device_id, device_name) in &status_data.devices {
            status_text.push_str("\n");
            status_text.push_str(&format!("{}\n", device_name));
            if let Some(statuses) = status_data.statuses.get(device_id) {
                for status_entry in statuses {
                    status_text.push_str(&format!("  [{:?}] {}\n", status_entry.level, status_entry.message));
                }
            } else {
                status_text.push_str("    No status entries.\n");
            }
        }

        Ok::<_, Infallible>(status_text)
    }
}
