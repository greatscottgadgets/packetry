//! Power controls.

use anyhow::Error;

use gtk::{
    prelude::*,
    ActionBar,
    CheckButton,
    DropDown,
    StringList,
    Switch,
};

use crate::backend::{BackendHandle, PowerConfig};

pub struct PowerControl {
    pub action_bar: ActionBar,
    pub controls: gtk::Box,
    pub switch: Switch,
    pub source_dropdown: DropDown,
    pub source_strings: StringList,
    pub start_on: CheckButton,
    pub stop_off: CheckButton,
}

impl PowerControl {
    pub fn connect_signals(&self, f: fn()) {
        let switch = self.switch.clone();
        let source_strings = self.source_strings.clone();
        self.source_dropdown.connect_selected_notify(move |dropdown| {
            update_tooltip(&switch, dropdown, &source_strings);
            f();
        });
        self.switch.connect_active_notify(move |_| f());
        self.start_on.connect_toggled(move |_| f());
        self.stop_off.connect_toggled(move |_| f());
    }

    pub fn update_controls(&self, device: Option<&mut Box<dyn BackendHandle>>) {
        if let Some(device) = device {
            match (device.power_sources(), device.power_config()) {
                (Some(sources), Some(config)) => {
                    self.enable(sources);
                    self.set_config(config);
                },
                _ => self.disable()
            }
        } else {
            self.disable();
        }
    }

    pub fn update_device(&self, device: Option<&mut Box<dyn BackendHandle>>)
        -> Result<(), Error>
    {
        if let Some(device) = device {
            device.set_power_config(self.config())?;
        }
        Ok(())
    }

    pub fn started(&self) {
        if self.enabled() && self.start_on.is_active() {
            self.switch.set_active(true);
        }
    }

    pub fn stopped(&self) {
        if self.enabled() && self.stop_off.is_active() {
            self.switch.set_active(false);
        }
    }

    fn enabled(&self) -> bool {
        self.controls.parent().is_some()
    }

    fn enable(&self, sources: &[&str]) {
        if !self.enabled() {
            self.action_bar.pack_start(&self.controls);
        }
        let old_len = self.source_strings.n_items();
        self.source_strings.splice(0, old_len, sources);
        update_tooltip(&self.switch, &self.source_dropdown, &self.source_strings);
    }

    fn disable(&self) {
        if self.enabled() {
            self.action_bar.remove(&self.controls);
        }
    }

    fn set_config(&self, config: PowerConfig) {
        self.source_dropdown.set_selected(config.source_index as u32);
        self.switch.set_active(config.on_now);
        self.start_on.set_active(config.start_on);
        self.stop_off.set_active(config.stop_off);
    }

    pub fn config(&self) -> PowerConfig {
        PowerConfig {
            source_index: self.source_dropdown.selected() as usize,
            on_now: self.switch.state(),
            start_on: self.start_on.is_active(),
            stop_off: self.stop_off.is_active(),
        }
    }
}

fn update_tooltip(switch: &Switch, dropdown: &DropDown, strings: &StringList) {
    switch.set_tooltip_text(
        strings
            .string(dropdown.selected())
            .map(|gstr| format!(
                "Controls VBUS passthrough from {} to TARGET-A. \
                 See power menu for more options.", gstr.as_str()))
            .as_deref()
    );
}
