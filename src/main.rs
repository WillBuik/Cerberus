use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use lazy_static::lazy_static;
use serde::{Serialize, Deserialize};

use crate::notification::{NotificationTarget, NotificationManager};
use crate::dummydevice::DummyDeviceMonitor;
use crate::status::{StatusManager, StatusLevel};

mod backgroundtask;
mod dummydevice;
mod notification;
mod status;

/// Cerberus monitor configration file format.
#[derive(Serialize, Deserialize, Debug)]
struct CerberusConfig {
    /// List of devices to monitor.
    devices: Vec<DeviceType>,

    /// Heartbeat time in seconds.
    /// 
    /// If no notification has been recently sent, a heartbeat status
    /// update will be sent after this timeout. Set to 0 to disable
    /// heartbeat notifications.
    notification_heartbeat: u64,

    /// Notification target for status updates.
    status_notification_target: Option<NotificationTarget>,

    /// Notification target for high-priority notifications.
    alarm_notification_target: Option<NotificationTarget>,
}

/// Cerberus monitor device configuration.
#[derive(Serialize, Deserialize, Debug)]
enum DeviceType {
    /// Dummy device for testing.
    /// 
    /// The dummy device cycles through its states at the
    /// configured rate starting from state 0, triggering status
    /// and alarm notifications and status updates.
    Dummy {
        /// List of states for the dummy to cycle through.
        /// 
        /// Each state is a tuple of a status string and whether or not
        /// that state is an alarm.
        states: Vec<(String, bool)>,

        /// Number of seconds between state changes updates.
        period: u64,
    },

    /// Napco Gemini alarm system.
    NapcoGemini {
        /// Serial port connected to the Napco Gemini communication bus.
        port: String,
    }
}

lazy_static! {
    /// Atomic device ID counter.
    static ref NEXT_DEVICE_ID: AtomicU64 = {
        AtomicU64::new(1)
    };
}

/// Unique ID for device monitors.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DeviceId (u64);

impl Default for DeviceId {
    /// Create a unique device monitor ID.
    fn default() -> Self {
        // Relaxed will be safe here because we are just using this as a
        // freestanding counter and subsequent fetch_adds are gaurenteed
        // to behave reasonably across threads.
        Self(NEXT_DEVICE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Common trait for device managers.
#[async_trait]
pub trait DeviceMonitor {
    /// Stop the device manager and wait for shutdown.
    async fn shutdown(&mut self);

    /// Get the device monitor's unique ID.
    fn id(&self) -> DeviceId;
}

/// Create a device monitor from a device configuration.
fn create_device_monitor(device_config: &DeviceType, status_manger: &StatusManager) -> anyhow::Result<Box<dyn DeviceMonitor>> {
    match device_config {
        DeviceType::Dummy { states, period } => {
            Ok(Box::new(DummyDeviceMonitor::new(status_manger.clone(), states.clone(), *period)?))
        },
        DeviceType::NapcoGemini { port: _ } => {
            anyhow::bail!("Napco Gemini device monitor not implmented");
        },
    }
}

/// Setup logging for the process.
fn setup_logging() {
    env_logger::builder().filter_level(log::LevelFilter::Info).init();
}


/// Return a path to the cerberus configuration file if it exists.
fn config_path() -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from("cerberus.json").canonicalize()?)
}

/// Load a cerberus configuration file.
fn load_configuration() -> anyhow::Result<CerberusConfig> {
    let config_data = std::fs::read(config_path()?)?;
    Ok(serde_json::from_slice(&config_data)?)
}

/// Attempt to read the notification target from a malformed cerberus
/// configuration file.
/// 
/// Returns a vector of found notification targets.
/// 
/// This is used to report startup failure if the configuration file
/// cannot be deserialized. This attempts to parse the configuration
/// as unstructured JSON first, if that fails it will attempt to
/// extract the targets using a regex. This allows limited, remote
/// error reporting even if the configuration file is badly malformed.
fn load_configruation_notification_targets() -> anyhow::Result<Vec<NotificationTarget>> {
    anyhow::bail!("not implemented");
}

#[tokio::main]
async fn main() {
    setup_logging();

    match config_path() {
        Ok(path) => {
            log::info!("Loading configuration from '{}'", path.to_string_lossy());
        },
        Err(_) => {
            log::error!("No configuration file found");
            std::process::exit(-1);
        },
    }

    let config = match load_configuration() {
        Ok(config) => config,
        Err(err) => {
            log::error!("Unable to parse configuration file: {}", err);

            // Todo: attempt to parse notification targets from file to send startup failure notice.

            std::process::exit(-1);
        },
    };

    let notification_manager = NotificationManager::new(config.status_notification_target.clone(), config.alarm_notification_target.clone());
    let status_manager = StatusManager::new(notification_manager.clone());

    status_manager.log("Cerberus monitor started.", StatusLevel::Status).await;

    // Send warnings to any available notification targets if status or alarm notification targets are not configured.
    if let None = config.status_notification_target {
        status_manager.log("No status notification target configured, status updates will not be sent.", StatusLevel::Warning).await;
    }
    if let None = config.alarm_notification_target {
        status_manager.log("No alarm notification target configured, alarm updates will not be sent!", StatusLevel::Warning).await;
    }

    // Start status server.
    if let Err(err) = status_manager.serve().await {
        status_manager.log(format!("Could not start status web server: {}", err), StatusLevel::Warning).await;
    }

    // Create device monitors.
    let mut devices: Vec<Box<dyn DeviceMonitor>> = vec![];
    for device_config in &config.devices {
        let device_monitor = create_device_monitor(device_config, &status_manager);
        match device_monitor {
            Ok(device_monitor) => {
                //todo log::info!("Created device monitor.");
                status_manager.register_device(&*device_monitor).await;
                devices.push(device_monitor)
            },
            Err(err) => {
                status_manager.log(format!("Could not create device monitor: {}", err), StatusLevel::Alarm).await;
            },
        }
    }

    // Wait for SIGTERM.
    let _ = tokio::spawn(async { tokio::signal::ctrl_c().await }).await;

    // Shut down device monitors.
    for mut device in devices {
        device.shutdown().await;
    }

    std::process::exit(0);
}
