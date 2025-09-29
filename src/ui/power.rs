//! Power controls.

use gtk::{
    prelude::*,
    glib::{self, SignalHandlerId},
    ActionBar,
    CheckButton,
    DropDown,
    StringList,
    Switch,
};

use crate::backend::{BackendHandle, PowerConfig};
use crate::ui::{display_error, with_ui};

pub struct PowerControl {
    pub action_bar: ActionBar,
    pub controls: gtk::Box,
    pub switch: Switch,
    pub source_dropdown: DropDown,
    pub source_strings: StringList,
    pub start_on: CheckButton,
    pub stop_off: CheckButton,
    pub signals: Option<PowerSignals>,
}

pub struct PowerSignals {
    pub switch_active: SignalHandlerId,
    pub start_on_toggled: SignalHandlerId,
    pub stop_off_toggled: SignalHandlerId,
    pub dropdown_selected: SignalHandlerId,
}

impl PowerControl {
    pub fn connect_signals(&self, f: fn()) {
        // Defer connection until UI setup is complete.
        let switch = self.switch.clone();
        let source_strings = self.source_strings.clone();
        let source_dropdown = self.source_dropdown.clone();
        let start_on = self.start_on.clone();
        let stop_off = self.stop_off.clone();
        glib::idle_add_local_once(move || {
            // Collect the signal IDs, we need these for blocking signals.
            let signals = PowerSignals {
                switch_active: switch.connect_active_notify(move |_| f()),
                start_on_toggled: start_on.connect_toggled(move |_| f()),
                stop_off_toggled: stop_off.connect_toggled(move |_| f()),
                dropdown_selected: source_dropdown.connect_selected_notify(
                    move |dropdown| {
                        update_tooltip(&switch, dropdown, &source_strings);
                        f();
                    }
                ),
            };
            // Store the collected signal IDs.
            display_error(with_ui(|ui| {
                ui.power.signals = Some(signals);
                Ok(())
            }));
        });
    }

    fn block_signals(&self) {
        if let Some(signals) = &self.signals {
            self.switch.block_signal(&signals.switch_active);
            self.start_on.block_signal(&signals.start_on_toggled);
            self.stop_off.block_signal(&signals.stop_off_toggled);
            self.source_dropdown.block_signal(&signals.dropdown_selected);
        }
    }

    fn unblock_signals(&self) {
        if let Some(signals) = &self.signals {
            self.switch.unblock_signal(&signals.switch_active);
            self.start_on.unblock_signal(&signals.start_on_toggled);
            self.stop_off.unblock_signal(&signals.stop_off_toggled);
            self.source_dropdown.unblock_signal(&signals.dropdown_selected);
        }
    }

    pub fn update_controls(
        &self,
        device: Option<&mut Box<dyn BackendHandle>>,
        config: Option<PowerConfig>,
    ) {
        // We're overriding the state of these controls. We don't
        // want to emit the usual signals, as these would trigger
        // updates to the device as if a user had changed them.
        self.block_signals();

        if let Some(device) = device {
            match (device.power_sources(), config) {
                (Some(sources), Some(config)) => {
                    self.enable(sources);
                    self.set_config(config);
                },
                _ => self.disable()
            }
        } else {
            self.disable();
        }

        // Now we want the usual signals again when the user changes them.
        self.unblock_signals();
    }

    pub fn started(&self) {
        // The state was already changed on the device when the capture started,
        // so block signals while we update the control state.
        self.block_signals();

        if self.enabled() && self.start_on.is_active() {
            self.switch.set_active(true);
        }

        self.unblock_signals();
    }

    pub fn stopped(&self) {
        // The state was already changed on the device when the capture stopped,
        // so block signals while we update the control state.
        self.block_signals();

        if self.enabled() && self.stop_off.is_active() {
            self.switch.set_active(false);
        }

        self.unblock_signals();
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
