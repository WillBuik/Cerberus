use std::time::Duration;
use std::str;
use async_trait::async_trait;
use serialport::SerialPort;

use crate::DeviceId;
use crate::DeviceMonitor;
use crate::backgroundtask::BackgroundTask;
use crate::status::StatusLevel;
use crate::status::StatusManager;

/// Interface to recieve messages from a Napco Gemini serial communication bus.
struct NapcoSerialInterface {
    port: Box<dyn SerialPort>,

    // Buffer for incoming bytes of the serial port, may contain multiple or incomplete messages.
    buffer: Vec<u8>,

    // Bytes currently in the buffer.
    buffer_len: usize,

    // Number of bytes discarded because they didn't belong to a valid message.
    error_count: u64,
}

impl NapcoSerialInterface {
    /// Buffer capacity.
    const BUFFER_CAP: usize = 1024;
    
    /// Serial baud rate for Napco Gemini bus.
    const NAPCO_GEMINI_BAUD: u32 = 5200;

    /// Port read timeout in milliseconds.
    const PORT_TIMEOUT_MS: u64 = 10;

    /// Create a new NapcoSerialMonitor for a Gemini bus on port.
    pub fn new(port: &str) -> serialport::Result<NapcoSerialInterface> {
        let port = serialport::new(port, Self::NAPCO_GEMINI_BAUD)
            .timeout(Duration::from_millis(Self::PORT_TIMEOUT_MS))
            .open()?;

        return Ok(NapcoSerialInterface {
            port,
            buffer: vec![0; Self::BUFFER_CAP],
            buffer_len: 0,
            error_count: 0,
        });
    }

    /// Advance the buffer, discarding n bytes.
    fn discard_buffer(&mut self, n: usize, is_error: bool) {
        if n > self.buffer_len {
            panic!("Attempted to discard non-existant data");
        }

        for i in n..self.buffer_len {
            self.buffer[i - n] = self.buffer[i];
        }

        self.buffer_len -= n;
        if is_error {
            self.error_count += n as u64;
        }
    }

    /// Reads one message from the serial port or returns None if a
    /// complete message hasn't been recieved yet.
    pub fn read_message_vec(&mut self) -> Option<Vec<u8>> {
        //Read any pending data at the port into the buffer unless it is full.
        if self.buffer_len < self.buffer.len() {
            let read_len = self.port.read(&mut self.buffer[self.buffer_len..]);
            if let Ok(read_len) = read_len {
                self.buffer_len += read_len;
            }
        }

        // Check if there is a complete message in the buffer.
        // Need at least 4 bytes minimal message.
        // ????????.???LLLLL.[MESSAGE]+.[CHECKSUM]
        while self.buffer_len >= 4 {
            let message_length = (self.buffer[1] & 0x1F) as usize;

            if message_length < 3 {
                // This message's length isn't valid - move the window forward and try again.
                //println!("l {:02X?}", self.buffer[0]);
                self.discard_buffer(1, true);
                continue;
            }

            if self.buffer_len < message_length {
                return None; // Haven't loaded the whole message into the buffer yet.
            }

            let message_checksum = self.buffer[message_length - 1];
            let mut recieved_checksum = 0u8;
            for i in 0..message_length-1 {
                let (x, _) = recieved_checksum.overflowing_add(self.buffer[i]);
                recieved_checksum = x;
            }

            if message_checksum != recieved_checksum {
                // This message's checksum isn't valid - move the window forward and try again.
                //println!("c {:02X?}", self.buffer[0]);
                self.discard_buffer(1, true);
                continue;
            }

            let message = self.buffer[0..message_length].to_vec();
            self.discard_buffer(message_length, false);
            return Some(message);
        }

        return None;
    }

    fn keypad_status(status1: u8, status2: u8) -> String {
        let status = match (status1, status2) {
            (0x02, 0x00) => Some("Ready"),
            (0x06, 0x00) => Some("Ready, Bypass"),
            (0x00, 0x00) => Some("Zone Fault"),
            (0x04, 0x00) => Some("Zone Fault, Bypass"),

            (0x85, 0x80) => Some("Arming, Bypass"),
            (0x05, 0x80) => Some("Armed, Bypass"),
            (0xC5, 0x80) => Some("Disarm, Bypass"),
            (0xC5, 0xC0) => Some("Disarm, Bypass"), // Fast beep, 10 seconds left
            (0x45, 0x81) => Some("ALARM, Bypass"),

            (0x81, 0x80) => Some("Arming"),
            (0x01, 0x80) => Some("Armed"),
            (0xC1, 0x80) => Some("Disarm"),
            (0xC1, 0xC0) => Some("Disarm"), // Fast beep, 10 seconds left
            (0x41, 0x81) => Some("ALARM"),
            
            (0x85, 0x90) => Some("Arming, Instant, Bypass"),
            (0x05, 0x90) => Some("Armed, Instant, Bypass"),
            (0x81, 0x90) => Some("Arming, Instant"),
            (0x01, 0x90) => Some("Armed, Instant"),

            // Todo: ALARM, but beeping has been silanced after 15 minutes.
            (_, _) => None,
        };

        return match status {
            Some(status) => status.to_string(),
            None => format!("Unknown ({:02X?},{:02X?})", status1, status2),
        }
    }

    /// Attempt to decode a message from the panel to the keypad.
    /// If it is a keypad message, returns
    /// Some(keypad status, keypad line, keypad text).
    /// 
    /// Keypad text line 0 and 1 are sent as seperate messages.
    /// 
    /// Warning! This has some pretty major pitfalls. I have not yet
    /// completely reverse engineered the bus protocol, but this should
    /// sucessfully decode messages sent to the primary keypad in a
    /// single area system. Beyond that, use at your own risk.
    pub fn decode_keypad_message(message: &[u8]) -> Option<(String, i8, String)> {
        if message.len() == 27 && message[4] == 0x01 {
            //log::info!("Bytes recv {:02X?} ({})", message, message.len());

            let line = if message[5] == 0x20 {
                0
            } else if message[5] == 0x60 {
                1
            } else {
                -1
            };

            if line >= 0 {
                let keypad_status = Self::keypad_status(message[8], message[9]);
                let keypad_text = String::from_utf8_lossy(&message[10..26]).to_string();
                return Some((keypad_status, line, keypad_text));
            }
        }
        None
    }
}

/// Device monitor for a Napco Gemini alarm panel.
pub struct NapcoGeminiDeviceMonitor {
    /// Unique device ID.
    id: DeviceId,

    /// Background task to monitor the serial communication bus.
    monitor_task: BackgroundTask<()>,
}

impl NapcoGeminiDeviceMonitor {
    pub fn new(status_manger: StatusManager, serial_port: String) -> anyhow::Result<Self> {
        let id = Default::default();

        let monitor_task = BackgroundTask::try_spawn(|shutdown_token| {
            let mut serial_interface = NapcoSerialInterface::new(&serial_port)?;

            Ok::<_, anyhow::Error>(async move {
                status_manger.update_status(id, "Napco Gemini device monitor started.", StatusLevel::Info).await;

                let mut last_line_0 = None;
                let mut last_keypad_message = String::new();

                while !shutdown_token.is_cancelled() {
                    // Read a message off the bus.
                    if let Some(message) = serial_interface.read_message_vec() {
                        if let Some((keypad_status, keypad_line, keypad_text)) = NapcoSerialInterface::decode_keypad_message(&message) {
                            if keypad_line == 0 {
                                // Store the first line of the message.
                                last_line_0 = Some(keypad_text);
                            } else {
                                // Merge second line of message with first into a status update.
                                if let Some(last_line) = last_line_0 {
                                    let keypad_entire_text = format!("{} {}", last_line.trim(), keypad_text.trim()).trim().to_string();
                                    let keypad_message = format!("{} \"{}\"", keypad_status, keypad_entire_text);
                                    if keypad_message != last_keypad_message {
                                        let level = if keypad_message.to_lowercase().contains("alarm") {
                                            StatusLevel::Alarm
                                        } else {
                                            StatusLevel::Status
                                        };
                                        status_manger.update_status(id, &keypad_message, level).await;
                                        last_keypad_message = keypad_message;
                                    }
                                } else {
                                    // Something went wrong, maybe a message was corrupted.
                                    log::warn!("Recieved keypad line 1 without line 0");
                                }
                                last_line_0 = None;
                            }
                        }
                    }

                    // This entire task is sync until it sends a status update, let the executor tick.
                    tokio::task::yield_now().await;
                }

                status_manger.update_status(id, "Napco Gemini device monitor stopped.", StatusLevel::Info).await;
            })
        })?;

        Ok(Self {
            id,
            monitor_task,
        })
    }
}

#[async_trait]
impl DeviceMonitor for NapcoGeminiDeviceMonitor {
    /// Shutdown the device monitoring loop.
    async fn shutdown(&mut self) {
        let _ = self.monitor_task.finish().await;
    }

    /// Get device ID.
    fn id(&self) -> crate::DeviceId {
        self.id
    }
}
