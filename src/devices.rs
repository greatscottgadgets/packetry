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

	let count = devices.len();
	let mut dev_names = Vec::with_capacity(count);
	let mut dev_serialnumbers = Vec::with_capacity(count);
	let mut dev_states = Vec::with_capacity(count);
	let mut dev_busnumbers = Vec::with_capacity(count);
	let mut dev_addresses = Vec::with_capacity(count);
	let mut dev_speeds = Vec::with_capacity(count);

	for device in devices.iter() {
		let info = &device.device_info;
		
		dev_names.push(
			// Maybe in the future there are more devices to support. Hardcode the name for now.
			"Cynthion".to_string()
		);

		
		dev_serialnumbers.push(
			if let Some(serial) = info.serial_number() {
				serial.to_string()
			} else {
				"None".to_string()
			}
		);

		dev_busnumbers.push(info.bus_number().to_string());
		dev_addresses.push(info.device_address().to_string());

		match &device.usability {
			Usable(_, speeds) => {
				dev_states.push("Yes".to_string());
				dev_speeds.push(
					speeds.iter()
						.map(|speed| {
							let desc = speed.description();
							desc.split(" ").next().unwrap_or("").to_string()
						})
						.collect()
				);
			},
			
			Unusable(reason) => {
				dev_states.push(reason.to_string());
				dev_speeds.push(Vec::new());
			}
		}
	}

	let mut device_table = Vec::with_capacity(count);
	for i in 0..count {
		device_table.push(DeviceInfo {
			name: dev_names[i].clone(),
			serial: dev_serialnumbers[i].clone(),
			useable: dev_states[i].clone(),
			bus: dev_busnumbers[i].clone(),
			address: dev_addresses[i].clone(),
			speeds: dev_speeds[i].join(", "),
		});
	}

	let table = Table::new(device_table).to_string();
	println!("{}", table);

	Ok(())
}