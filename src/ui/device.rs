//! UI for selecting and displaying info about capture devices.

use std::cell::{RefCell, Ref, RefMut};
use std::ops::{Deref, DerefMut};

use gtk::{
    glib::{self, prelude::*},
    gio::{self, subclass::prelude::*},
    prelude::*,
    ClosureExpression,
    DropDown,
    Expression,
    InfoBar,
    Label,
    MessageType,
    ResponseType,
    StringList,
};

use anyhow::{bail, Error};
use futures_lite::StreamExt;
use indexmap::IndexMap;
use nusb::{DeviceId, hotplug::{HotplugEvent, HotplugWatch}};

use crate::backend::{BackendHandle, ProbeResult, scan, probe};
use crate::usb::Speed;

pub struct ActiveDevice {
    handle: Box<dyn BackendHandle>,
    speeds: Vec<Speed>,
}

pub struct DeviceSelector {
    devices: DeviceList,
    dev_dropdown: DropDown,
    speed_dropdown: DropDown,
    active: Option<Result<ActiveDevice, String>>,
}

impl DeviceSelector {
    pub fn new(dev_dropdown: DropDown, speed_dropdown: DropDown)
        -> Result<Self, Error>
    {
        let devices = DeviceList::new()?;

        dev_dropdown.set_expression(Some(devices.description_expression()));

        dev_dropdown.set_model(Some(&devices));
        speed_dropdown.set_model(Some(&StringList::new(&[])));

        let selector = DeviceSelector {
            devices: devices.clone(),
            dev_dropdown: dev_dropdown.clone(),
            speed_dropdown,
            active: None,
        };

        devices.connect_items_changed(
            move |devices, _position, _removed, _added| {
                dev_dropdown.set_expression(Some(devices.description_expression()));
            }
        );

        Ok(selector)
    }

    pub fn connect_signals(&self, f: fn()) {
        self.dev_dropdown.connect_selected_item_notify(move |_| f());
    }

    pub fn device_available(&self) -> bool {
        matches!(&self.active, Some(Ok(_)))
    }

    pub fn selected_device(&self) -> Option<Device> {
        if self.devices.is_empty() {
            None
        } else {
            self.dev_dropdown
                .selected_item()
                .and_then(|obj| obj.downcast().ok())
        }
    }

    pub fn device_unusable(&self) -> Option<String> {
        match &self.active {
            Some(Err(msg)) => Some(msg.clone()),
            _ => match &self.selected_device() {
                Some(dev) => match dev.probe_result().deref() {
                    ProbeResult {result: Err(msg), ..} => Some(msg.to_string()),
                    _ => None,
                },
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

    pub fn open_device(&mut self) -> Result<(), Error> {
        let mut speed_strings: Vec<&str> = Vec::new();
        self.active = if let Some(device) = self.selected_device() {
            if let Ok(backend_device) = &device
                .probe_result_mut()
                .deref_mut()
                .result
            {
                match backend_device.open_as_generic() {
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
        dropdown.set_model(Some(&StringList::new(&strings)));
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

    pub fn update(&self, warning: Option<String>) {
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

thread_local!(
    /// Singleton list model.
    static DEVICES: RefCell<Option<DeviceList>> = const { RefCell::new(None) };
);

/// Task to maintain list model.
async fn maintain_device_list(mut watch: HotplugWatch) {
    let list = DEVICES.with(|cell| cell.borrow().as_ref().unwrap().clone());
    loop {
        match watch.next().await {
            Some(HotplugEvent::Connected(info)) => {
                if let Some(probe_result) = probe(info) {
                    let (index, prev) = list.imp().devices
                        .borrow_mut()
                        .insert_full(
                            probe_result.info.id(),
                            Device::new(probe_result)
                        );
                    let removed = if prev.is_some() { 1 } else { 0 };
                    let added = 1;
                    list.items_changed(index as u32, removed, added);
                }
            },
            Some(HotplugEvent::Disconnected(id)) => {
                let mut devices = list.imp().devices.borrow_mut();
                if let Some((index, ..)) = devices
                    .shift_remove_full(&id)
                {
                    let removed = 1;
                    let added = 0;
                    drop(devices);
                    list.items_changed(index as u32, removed, added);
                }
            },
            None => return
        }
    }
}

// GLib wrapper for a single device

glib::wrapper! {
    pub struct Device(ObjectSubclass<DeviceInner>);
}

impl Device {
    fn new(probe_result: ProbeResult) -> Self {
        let device = glib::Object::new::<Device>();
        device.imp().probe_result.borrow_mut().replace(probe_result);
        device
    }

    fn probe_result(&self) -> Ref<'_, ProbeResult> {
        Ref::map(self.imp().probe_result.borrow(), |optref| optref.as_ref().unwrap())
    }

    fn probe_result_mut(&self) -> RefMut<'_, ProbeResult> {
        RefMut::map(self.imp().probe_result.borrow_mut(), |optref| optref.as_mut().unwrap())
    }
}

#[derive(Default)]
pub struct DeviceInner {
    probe_result: RefCell<Option<ProbeResult>>,
}

#[glib::object_subclass]
impl ObjectSubclass for DeviceInner {
    const NAME: &'static str = "Device";
    type Type = Device;
    type Interfaces = ();
}

impl ObjectImpl for DeviceInner {}

// GLib wrapper for a list of devices

glib::wrapper! {
    pub struct DeviceList(ObjectSubclass<DeviceListInner>)
        @implements gio::ListModel;
}

impl DeviceList {
    fn new() -> Result<Self, Error> {
        DEVICES.with(|cell| {
            let mut opt = cell.borrow_mut();
            if opt.is_none() {
                let list = glib::Object::new::<DeviceList>();
                glib::spawn_future_local(
                    maintain_device_list(
                        nusb::watch_devices()?
                    )
                );
                *list.imp().devices.borrow_mut() = scan()?
                    .into_iter()
                    .map(|probe| (probe.info.id(), Device::new(probe)))
                    .collect();
                *opt = Some(list)
            }
            Ok(opt.as_ref().unwrap().clone())
        })
    }

    fn is_empty(&self) -> bool {
        self.imp().n_items() == 0
    }

    fn description_expression(&self) -> ClosureExpression {
        ClosureExpression::new::<String>(
            Vec::<Expression>::new(),
            if self.n_items() == 1 {
                // Only one device in list. Display its name only.
                glib::closure!(|object: glib::Object| {
                    let device = object.downcast::<Device>().unwrap();
                    let probe = device.probe_result();
                    probe.name
                })
            } else {
                // Multiple devices in list. Show identifying details.
                glib::closure!(|object: glib::Object| {
                    let device = object.downcast::<Device>().unwrap();
                    let probe = device.probe_result();
                    if let Some(serial) = probe.info.serial_number() {
                        format!("{} #{}", probe.name, serial)
                    } else {
                        format!("{} (bus {}, device {})",
                            probe.name,
                            probe.info.bus_id(),
                            probe.info.device_address())
                    }
                })
            }
        )
    }
}

#[derive(Default)]
pub struct DeviceListInner {
    devices: RefCell<IndexMap<DeviceId, Device>>,
}

#[glib::object_subclass]
impl ObjectSubclass for DeviceListInner {
    const NAME: &'static str = "DeviceList";
    type Type = DeviceList;
    type Interfaces = (gio::ListModel,);
}

impl ObjectImpl for DeviceListInner {}

impl ListModelImpl for DeviceListInner {
    fn item_type(&self) -> glib::Type {
        Device::static_type()
    }

    fn n_items(&self) -> u32 {
        self.devices.borrow().len() as u32
    }

    fn item(&self, position: u32) -> Option<glib::Object> {
        self.devices
            .borrow()
            .get_index(position as usize)
            .map(|(_id, device)| device)
            .map(Device::upcast_ref)
            .cloned()
    }
}
