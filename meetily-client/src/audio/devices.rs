use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_input: bool,
    pub is_default: bool,
}

impl fmt::Display for AudioDeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.is_input { "input" } else { "output" };
        let default = if self.is_default { " default" } else { "" };
        write!(f, "[{direction}{default}] {}", self.name)
    }
}

pub fn list_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let default_input = host.default_input_device().and_then(|d| d.name().ok());
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    let mut devices = Vec::new();

    if let Ok(inputs) = host.input_devices() {
        for device in inputs {
            if let Ok(name) = device.name() {
                devices.push(AudioDeviceInfo {
                    is_default: default_input.as_deref() == Some(name.as_str()),
                    name,
                    is_input: true,
                });
            }
        }
    }

    if let Ok(outputs) = host.output_devices() {
        for device in outputs {
            if let Ok(name) = device.name() {
                devices.push(AudioDeviceInfo {
                    is_default: default_output.as_deref() == Some(name.as_str()),
                    name,
                    is_input: false,
                });
            }
        }
    }

    devices
}
