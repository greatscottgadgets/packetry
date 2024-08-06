use crate::backend::cynthion::{CynthionDevice, CynthionUsability::*};
use tabled::{Table, Tabled};
use anyhow::Result;

#[derive(Tabled)]
pub struct DeviceInfo {
    name: String,
    serial: String,
    useable: String,
    bus: String,
    address: String,
    speeds: String,
}

pub fn list_devices() -> Result<()> {
    let devices = CynthionDevice::scan()?;

    if devices.is_empty() {
        println!("No devices found.");
        return Ok(());
    }

    let device_table: Vec<DeviceInfo> = devices.iter().map(|device| {
        let info = &device.device_info;

        // Maybe in the future there are more devices to support. Hardcode the name for now.
        let name = "Cynthion".to_string(); 

        let serial = info.serial_number().map_or("None".to_string(), |s| s.to_string());
        let bus = info.bus_number().to_string();
        let address = info.device_address().to_string();

        let (useable, speeds) = match &device.usability {
            Usable(_, speeds) => (
                "Yes".to_string(),
                speeds.iter()
                    .map(|speed| {
                        let desc = speed.description();
                        desc.split(' ').next().unwrap_or("").to_string()
                    })
                    .collect::<Vec<String>>()
                    .join(", "),
            ),
            Unusable(reason) => (reason.to_string(), String::new()),
        };

        DeviceInfo {
            name,
            serial,
            useable,
            bus,
            address,
            speeds,
        }
    }).collect();

    let table = Table::new(device_table).to_string();
    println!("{}", table);

    Ok(())
}