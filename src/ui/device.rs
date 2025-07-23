//! UI for selecting and displaying info about capture devices.

use gtk::{
    prelude::*,
    glib::SignalHandlerId,
    DropDown,
    InfoBar,
    Label,
    MessageType,
    ResponseType,
    StringList,
};

use anyhow::{bail, Error};

use crate::backend::{BackendHandle, ProbeResult, scan};
use crate::ui::{display_error, device_selection_changed};
use crate::usb::Speed;

pub struct DeviceSelector {
    devices: Vec<ProbeResult>,
    dev_strings: Vec<String>,
    dev_speeds: Vec<Vec<&'static str>>,
    dev_dropdown: DropDown,
    speed_dropdown: DropDown,
    change_handler: Option<SignalHandlerId>,
}

impl DeviceSelector {
    pub fn new(dev_dropdown: DropDown, speed_dropdown: DropDown)
        -> Result<Self, Error>
    {
        dev_dropdown.set_model(Some(&StringList::new(&[])));
        speed_dropdown.set_model(Some(&StringList::new(&[])));
        Ok(DeviceSelector {
            devices: vec![],
            dev_strings: vec![],
            dev_speeds: vec![],
            dev_dropdown,
            speed_dropdown,
            change_handler: None,
        })
    }

    pub fn current_device(&self) -> Option<&ProbeResult> {
        if self.devices.is_empty() {
            None
        } else {
            Some(&self.devices[self.dev_dropdown.selected() as usize])
        }
    }

    pub fn device_available(&self) -> bool {
        match self.current_device() {
            None => false,
            Some(probe) => probe.result.is_ok()
        }
    }

    pub fn device_unusable(&self) -> Option<&str> {
        match self.current_device() {
            Some(ProbeResult {result: Err(msg), ..}) => Some(msg),
            _ => None
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
        if let Some(handler) = self.change_handler.take() {
            self.dev_dropdown.disconnect(handler);
        }
        self.devices = scan()?;
        let count = self.devices.len();
        self.dev_strings = Vec::with_capacity(count);
        self.dev_speeds = Vec::with_capacity(count);
        for probe in self.devices.iter() {
            self.dev_strings.push(
                if count <= 1 {
                    probe.name.to_string()
                } else {
                    let info = &probe.info;
                    if let Some(serial) = info.serial_number() {
                        format!("{} #{}", probe.name, serial)
                    } else {
                        format!("{} (bus {}, device {})",
                            probe.name,
                            info.bus_number(),
                            info.device_address())
                    }
                }
            );
            if let Ok(device) = &probe.result {
                self.dev_speeds.push(
                    device
                        .supported_speeds()
                        .iter()
                        .map(Speed::description)
                        .collect()
                );
            } else {
                self.dev_speeds.push(vec![]);
            }
        }
        let no_speeds = vec![];
        let speed_strings = self.dev_speeds.first().unwrap_or(&no_speeds);
        self.replace_dropdown(&self.dev_dropdown, &self.dev_strings);
        self.replace_dropdown(&self.speed_dropdown, speed_strings);
        self.dev_dropdown.set_sensitive(!self.devices.is_empty());
        self.speed_dropdown.set_sensitive(!speed_strings.is_empty());
        self.change_handler = Some(
            self.dev_dropdown.connect_selected_notify(
                |_| display_error(device_selection_changed())));
        Ok(())
    }

    pub fn update_speeds(&self) {
        let index = self.dev_dropdown.selected() as usize;
        let speed_strings = &self.dev_speeds[index];
        self.replace_dropdown(&self.speed_dropdown, speed_strings);
        self.speed_dropdown.set_sensitive(!speed_strings.is_empty());
    }

    pub fn open(&self) -> Result<(Box<dyn BackendHandle>, Speed), Error> {
        let device_id = self.dev_dropdown.selected();
        let probe = &self.devices[device_id as usize];
        match &probe.result {
            Ok(device) => {
                let speeds = device.supported_speeds();
                let speed_id = self.speed_dropdown.selected() as usize;
                let speed = speeds[speed_id];
                let handle = device.open_as_generic()?;
                Ok((handle, speed))
            },
            Err(reason) => {
                bail!("Device not usable: {}", reason)
            }
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
