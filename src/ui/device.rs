//! UI for selecting and displaying info about capture devices.

use gtk::{
    prelude::*,
    DropDown,
    InfoBar,
    Label,
    MessageType,
    ResponseType,
    StringList,
};

use anyhow::{bail, Error};

use crate::backend::{BackendHandle, ProbeResult, scan};
use crate::usb::Speed;

pub struct ActiveDevice {
    handle: Box<dyn BackendHandle>,
    speeds: Vec<Speed>,
}

pub struct DeviceSelector {
    devices: Vec<ProbeResult>,
    dev_dropdown: DropDown,
    speed_dropdown: DropDown,
    active: Option<Result<ActiveDevice, String>>,
}

impl DeviceSelector {
    pub fn new(dev_dropdown: DropDown, speed_dropdown: DropDown)
        -> Result<Self, Error>
    {
        dev_dropdown.set_model(Some(&StringList::new(&[])));
        speed_dropdown.set_model(Some(&StringList::new(&[])));

        Ok(DeviceSelector {
            devices: vec![],
            dev_dropdown,
            speed_dropdown,
            active: None,
        })
    }

    pub fn connect_signals(&self, f: fn()) {
        self.dev_dropdown.connect_selected_notify(move |_| f());
    }

    pub fn device_available(&self) -> bool {
        matches!(&self.active, Some(Ok(_)))
    }

    pub fn selected_device(&self) -> Option<&ProbeResult> {
        if self.devices.is_empty() {
            None
        } else {
            let index = self.dev_dropdown.selected() as usize;
            Some(&self.devices[index])
        }
    }

    fn selected_device_mut(&mut self) -> Option<&mut ProbeResult> {
        if self.devices.is_empty() {
            None
        } else {
            let index = self.dev_dropdown.selected() as usize;
            Some(&mut self.devices[index])
        }
    }

    pub fn device_unusable(&self) -> Option<&str> {
        match &self.active {
            Some(Err(msg)) => Some(msg),
            _ => match &self.selected_device() {
                Some(ProbeResult {result: Err(msg), ..}) => Some(msg),
                _ => None
            }
        }
    }

    pub fn set_sensitive(&mut self, sensitive: bool) {
        if sensitive {
            self.dev_dropdown.set_sensitive(!self.devices.is_empty());
            self.speed_dropdown.set_sensitive(self.device_available());
        } else {
            self.dev_dropdown.set_sensitive(false);
            self.speed_dropdown.set_sensitive(false);
        }
    }

    pub fn scan(&mut self) -> Result<(), Error> {
        self.active = None;
        self.devices = scan()?;
        let count = self.devices.len();
        let dev_strings = self.devices
            .iter()
            .map(|probe|
                if count <= 1 {
                    probe.name.to_string()
                } else {
                    let info = &probe.info;
                    if let Some(serial) = info.serial_number() {
                        format!("{} #{}", probe.name, serial)
                    } else {
                        format!("{} (bus {}, device {})",
                            probe.name,
                            info.bus_id(),
                            info.device_address())
                    }
                }
            )
            .collect::<Vec<String>>();
        self.replace_dropdown(&self.dev_dropdown, &dev_strings);
        self.dev_dropdown.set_sensitive(!self.devices.is_empty());
        self.speed_dropdown.set_sensitive(!self.devices.is_empty());
        Ok(())
    }

    pub fn open_device(&mut self) -> Result<(), Error> {
        let mut speed_strings: Vec<&str> = Vec::new();
        self.active = if let Some(probe) = self.selected_device_mut() {
            if let Ok(device) = &mut probe.result {
                match device.open_as_generic() {
                    Ok(handle) => {
                        let speeds = handle.supported_speeds().to_vec();
                        speed_strings.extend(
                            speeds.iter().map(Speed::description)
                        );
                        Some(Ok(ActiveDevice { handle, speeds }))
                    },
                    Err(error) => {
                        Some(Err(format!("{error}")))
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        self.replace_dropdown(&self.speed_dropdown, &speed_strings);
        self.speed_dropdown.set_sensitive(!speed_strings.is_empty());
        Ok(())
    }

    pub fn handle(&mut self) -> Option<&mut Box<dyn BackendHandle>> {
        if let Some(Ok(ActiveDevice { handle, .. })) = &mut self.active {
            Some(handle)
        } else {
            None
        }
    }

    pub fn handle_and_speed(&self)
        -> Result<(Box<dyn BackendHandle>, Speed), Error>
    {
        if let Some(Ok(ActiveDevice { handle, speeds })) = &self.active {
            let speed_id = self.speed_dropdown.selected() as usize;
            let speed = speeds[speed_id];
            Ok((handle.duplicate(), speed))
        } else {
            bail!("No active device handle");
        }
    }

    fn replace_dropdown<T: AsRef<str>>(
        &self, dropdown: &DropDown, strings: &[T])
    {
        let strings = strings
            .iter()
            .map(T::as_ref)
            .collect::<Vec<_>>();
        if let Some(model) = dropdown.model() {
            let num_items = model.n_items();
            if let Ok(list) = model.downcast::<StringList>() {
                list.splice(0, num_items, strings.as_slice());
            }
        }
    }
}

pub struct DeviceWarning {
    info_bar: InfoBar,
    label: Label,
}

impl DeviceWarning {
    pub fn new(info_bar: InfoBar, label: Label) -> DeviceWarning {
        info_bar.connect_response(|info_bar, response| {
            if response == ResponseType::Close {
                info_bar.set_revealed(false);
            }
        });
        DeviceWarning {
            info_bar,
            label,
        }
    }

    pub fn update(&self, warning: Option<&str>) {
        if let Some(reason) = warning {
            self.info_bar.set_message_type(MessageType::Warning);
            self.label.set_text(&format!(
                "This device is not usable because: {reason}"));
            self.info_bar.set_revealed(true);
        } else {
            self.info_bar.set_revealed(false);
        }
    }
}
