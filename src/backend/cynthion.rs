//! USB capture backend for Cynthion.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::ops::DerefMut;
use std::time::Duration;
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{Context as ErrorContext, Error, bail};
use nusb::{
    self,
    transfer::{
        Control,
        ControlType,
        Recipient,
    },
    DeviceInfo,
    Interface
};

use super::{
    BackendDevice,
    BackendHandle,
    Speed,
    PacketIterator,
    PacketResult,
    PowerConfig,
    TimestampedPacket,
    TransferQueue,
};

use crate::capture::CaptureMetadata;

pub const VID_PID: (u16, u16) = (0x1d50, 0x615b);
const CLASS: u8 = 0xff;
const SUBCLASS: u8 = 0x10;
const PROTOCOL: u8 = 0x01;
const ENDPOINT: u8 = 0x81;
const READ_LEN: usize = 0x4000;
const NUM_TRANSFERS: usize = 4;

bitfield! {
    #[derive(Copy, Clone)]
    struct State(u8);
    bool, enable, set_enable: 0;
    u8, from into Speed, speed, set_speed: 2, 1;
    bool, target_c_vbus_en, set_target_c_vbus_en: 3;
    bool, control_vbus_en, set_control_vbus_en: 4;
    bool, aux_vbus_en, set_aux_vbus_en: 5;
    bool, target_a_discharge, set_target_a_discharge: 6;
    bool, power_control_enable, set_power_control_enable: 7;
}

bitfield! {
    #[derive(Copy, Clone)]
    struct TestConfig(u8);
    bool, connect, set_connect: 0;
    u8, from into Speed, speed, set_speed: 2, 1;
}

impl TestConfig {
    fn new(speed: Option<Speed>) -> TestConfig {
        let mut config = TestConfig(0);
        match speed {
            Some(speed) => {
                config.set_connect(true);
                config.set_speed(speed);
            },
            None => {
                config.set_connect(false);
            }
        };
        config
    }
}

/// A Cynthion device attached to the system.
pub struct CynthionDevice {
    pub device_info: DeviceInfo,
}

/// Fields of CynthionHandle to be stored in a mutex.
struct CynthionInner {
    interface: Interface,
    state: State,
    power: Option<PowerConfig>,
}

/// A handle to an open Cynthion device.
#[derive(Clone)]
pub struct CynthionHandle {
    inner: Arc<Mutex<CynthionInner>>,
    speeds: Vec<Speed>,
    metadata: CaptureMetadata,
    power_sources: Option<&'static [&'static str]>,
}

/// Converts from received data bytes to timestamped packets.
pub struct CynthionStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
    padding_due: bool,
    total_clk_cycles: u64,
}

/// Convert 60MHz clock cycles to nanoseconds, rounding down.
fn clk_to_ns(clk_cycles: u64) -> u64 {
    const TABLE: [u64; 3] = [0, 16, 33];
    let quotient = clk_cycles / 3;
    let remainder = clk_cycles % 3;
    quotient * 50 + TABLE[remainder as usize]
}

/// Probe a Cynthion device.
pub fn probe(device_info: DeviceInfo) -> Result<Box<dyn BackendDevice>, Error> {
    Ok(Box::new(CynthionDevice {device_info }))
}

impl CynthionDevice {
    /// Open this device.
    pub fn open(&self) -> Result<CynthionHandle, Error> {
        use Speed::*;

        // Check we can open the device.
        let device = self.device_info
            .open()
            .context("Failed to open device")?;

        // Read the active configuration.
        let config = device
            .active_configuration()
            .context("Failed to retrieve active configuration")?;

        // Iterate over the interfaces...
        for interface in config.interfaces() {
            let interface_number = interface.interface_number();

            // ...and alternate settings...
            for alt_setting in interface.alt_settings() {
                let alt_setting_number = alt_setting.alternate_setting();

                // Ignore if this is not our supported target.
                if alt_setting.class() != CLASS ||
                   alt_setting.subclass() != SUBCLASS
                {
                    continue;
                }

                // Check protocol version.
                let protocol = alt_setting.protocol();
                #[allow(clippy::absurd_extreme_comparisons)]
                match PROTOCOL.cmp(&protocol) {
                    Ordering::Less =>
                        bail!("Analyzer gateware is newer (v{}) than supported by this version of Packetry (v{}). Please update Packetry.", protocol, PROTOCOL),
                    Ordering::Greater =>
                        bail!("Analyzer gateware is older (v{}) than supported by this version of Packetry (v{}). Please update gateware.", protocol, PROTOCOL),
                    Ordering::Equal => {}
                }

                // Try to claim the interface.
                let interface = device
                    .claim_interface(interface_number)
                    .context("Failed to claim interface")?;

                // Select the required alternate, if not the default.
                if alt_setting_number != 0 {
                    interface
                        .set_alt_setting(alt_setting_number)
                        .context("Failed to select alternate setting")?;
                }

                // Read the state register.
                let mut state = State(read_byte(&interface, 0)?);

                // Fetch the available speeds.
                let mut speeds = Vec::new();
                let speed_byte = read_byte(&interface, 2)?;
                for speed in [Auto, High, Full, Low] {
                    if speed_byte & speed.mask() != 0 {
                        speeds.push(speed);
                    }
                }

                // Fetch the minor protocol version.
                let protocol_minor = read_byte(&interface, 4).unwrap_or(0);

                let metadata = CaptureMetadata {
                    iface_desc: Some("Cynthion USB Analyzer".to_string()),
                    iface_hardware: Some({
                        let bcd = self.device_info.device_version();
                        let major = bcd >> 8;
                        let minor = bcd as u8;
                        format!("Cynthion r{major}.{minor}")
                    }),
                    iface_os: Some(
                        format!("USB Analyzer v{protocol}.{protocol_minor}")),
                    iface_snaplen: Some(NonZeroU32::new(0xFFFF).unwrap()),
                    .. Default::default()
                };

                // Translate the power configuration.
                let power = if (protocol, protocol_minor) < (1, 1) {
                    // Analyzer does not support power control.
                    state.set_power_control_enable(false);
                    None
                } else {
                    // Analyzer supports power control.
                    let (source_index, on_now) =
                        if !state.power_control_enable() {
                            // Power control has not yet been set up.
                            // Set the initial configuration.
                            state.set_power_control_enable(true);
                            state.set_target_c_vbus_en(true);
                            state.set_control_vbus_en(false);
                            state.set_aux_vbus_en(false);
                            state.set_target_a_discharge(false);
                            (0, true)
                        } else if state.target_c_vbus_en() {
                            (0, true)
                        } else if state.control_vbus_en() {
                            (1, true)
                        } else if state.aux_vbus_en() {
                            (2, true)
                        } else {
                            (0, false)
                        };
                    Some(PowerConfig {
                        source_index,
                        on_now,
                        start_on: false,
                        stop_off: false,
                    })
                };

                let power_sources: Option<&[&str]> = if power.is_none() {
                    None
                } else if self.device_info.device_version() >= 0x0006 {
                    Some(&["TARGET-C", "CONTROL", "AUX"])
                } else {
                    Some(&["TARGET-C", "HOST"])
                };

                // Now we have a usable device.
                return Ok(CynthionHandle {
                    inner: Arc::new(Mutex::new(
                        CynthionInner {
                            interface,
                            state,
                            power,
                        }
                    )),
                    speeds,
                    metadata,
                    power_sources,
                })
            }
        }

        bail!("No supported analyzer interface found");
    }
}

impl BackendDevice for CynthionDevice {
    fn open_as_generic(&self) -> Result<Box<dyn BackendHandle>, Error> {
        Ok(Box::new(self.open()?))
    }
}

impl BackendHandle for CynthionHandle {
    fn supported_speeds(&self) -> &[Speed] {
        &self.speeds
    }

    fn metadata(&self) -> &CaptureMetadata {
        &self.metadata
    }

    fn power_sources(&self) -> Option<&'static [&'static str]> {
        self.power_sources
    }

    fn power_config(&self) -> Option<PowerConfig> {
        self.inner().power.clone()
    }

    fn set_power_config(&mut self, power: PowerConfig) -> Result<(), Error> {
        self.inner().set_power_config(power)
    }

    fn begin_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Vec<u8>>
    ) -> Result<TransferQueue, Error>
    {
        let mut inner = self.inner();

        inner.start_capture(speed)?;

        Ok(TransferQueue::new(&inner.interface, data_tx,
            ENDPOINT, NUM_TRANSFERS, READ_LEN))
    }

    fn end_capture(&mut self) -> Result<(), Error> {
        self.inner().stop_capture()
    }

    fn post_capture(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn timestamped_packets(&self, data_rx: mpsc::Receiver<Vec<u8>>)
        -> Box<dyn PacketIterator>
    {
        Box::new(
            CynthionStream {
                receiver: data_rx,
                buffer: VecDeque::new(),
                padding_due: false,
                total_clk_cycles: 0,
            }
        )
    }

    fn duplicate(&self) -> Box<dyn BackendHandle> {
        Box::new(self.clone())
    }
}

impl CynthionInner {
    fn start_capture (&mut self, speed: Speed) -> Result<(), Error> {
        self.state.set_speed(speed);
        self.state.set_enable(true);
        if let Some(power) = &mut self.power {
            if power.start_on {
                let index = power.source_index;
                self.state.set_target_c_vbus_en(index == 0);
                self.state.set_control_vbus_en(index == 1);
                self.state.set_aux_vbus_en(index == 2);
                self.state.set_target_a_discharge(false);
                power.on_now = true;
            }
        }
        self.write_request(1, self.state.0)
    }

    fn stop_capture(&mut self) -> Result<(), Error> {
        self.state.set_enable(false);
        if let Some(power) = &mut self.power {
            if power.stop_off {
                self.state.set_target_c_vbus_en(false);
                self.state.set_control_vbus_en(false);
                self.state.set_aux_vbus_en(false);
                self.state.set_target_a_discharge(true);
                power.on_now = false;
            }
        }
        self.write_request(1, self.state.0)
    }

    fn set_power_config(&mut self, power: PowerConfig) -> Result<(), Error> {
        let index = power.source_index;
        let on = power.on_now;
        self.state.set_power_control_enable(true);
        self.state.set_target_c_vbus_en(on && index == 0);
        self.state.set_control_vbus_en(on && index == 1);
        self.state.set_aux_vbus_en(on && index == 2);
        self.state.set_target_a_discharge(!on);
        self.power = Some(power);
        self.write_request(1, self.state.0)
    }

    fn write_request(&mut self, request: u8, value: u8) -> Result<(), Error> {
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value: u16::from(value),
            index: self.interface.interface_number() as u16,
        };
        let data = &[];
        let timeout = Duration::from_secs(1);
        self.interface
            .control_out_blocking(control, data, timeout)
            .context("Write request failed")?;
        Ok(())
    }
}

impl CynthionHandle {
    fn inner(&self) -> impl DerefMut<Target=CynthionInner> + use<'_> {
        self.inner.lock().unwrap()
    }

    pub fn configure_test_device(&mut self, speed: Option<Speed>)
        -> Result<(), Error>
    {
        let test_config = TestConfig::new(speed);
        self.inner()
            .write_request(3, test_config.0)
            .context("Failed to set test device configuration")
    }
}

fn read_byte(interface: &Interface, request: u8) -> Result<u8, Error> {
    let control = Control {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request,
        value: 0,
        index: interface.interface_number() as u16,
    };
    let mut buf = [0; 64];
    let timeout = Duration::from_secs(1);
    let size = interface
        .control_in_blocking(control, &mut buf, timeout)
        .context("Failed retrieving supported speeds from device")?;
    if size != 1 {
        bail!("Expected 1-byte response, got {size}");
    }
    Ok(buf[0])
}


impl PacketIterator for CynthionStream {}

impl Iterator for CynthionStream {
    type Item = PacketResult;
    fn next(&mut self) -> Option<PacketResult> {
        loop {
            // Do we have another packet already in the buffer?
            match self.next_buffered_packet() {
                // Yes; return the packet.
                Some(packet) => return Some(Ok(packet)),
                // No; wait for more data from the capture thread.
                None => match self.receiver.recv().ok() {
                    // Received more data; add it to the buffer and retry.
                    Some(bytes) => self.buffer.extend(bytes.iter()),
                    // Capture has ended, there are no more packets.
                    None => return None
                }
            }
        }
    }
}

impl CynthionStream {
    fn next_buffered_packet(&mut self) -> Option<TimestampedPacket> {
        // Are we waiting for a padding byte?
        if self.padding_due {
            if self.buffer.is_empty() {
                return None;
            } else {
                self.buffer.pop_front();
                self.padding_due= false;
            }
        }

        // Loop over any non-packet events, until we get to a packet.
        loop {
            // Do we have the length and timestamp for the next packet/event?
            if self.buffer.len() < 4 {
                return None;
            }

            if self.buffer[0] == 0xFF {
                // This is an event.
                let _event_code = self.buffer[1];

                // Update our cycle count.
                self.update_cycle_count();

                // Remove event from buffer.
                self.buffer.drain(0..4);
            } else {
                // This is a packet, handle it below.
                break;
            }
        }

        // Do we have all the data for the next packet?
        let packet_len = u16::from_be_bytes(
            [self.buffer[0], self.buffer[1]]) as usize;
        if self.buffer.len() <= 4 + packet_len {
            return None;
        }

        // Update our cycle count.
        self.update_cycle_count();

        // Remove the length and timestamp from the buffer.
        self.buffer.drain(0..4);

        // If packet length is odd, we will need to skip a padding byte after.
        if packet_len % 2 == 1 {
            self.padding_due = true;
        }

        // Remove the rest of the packet from the buffer and return it.
        Some(TimestampedPacket {
            timestamp_ns: clk_to_ns(self.total_clk_cycles),
            bytes: self.buffer.drain(0..packet_len).collect()
        })
    }

    fn update_cycle_count(&mut self) {
        // Decode the cycle count.
        let clk_cycles = u16::from_be_bytes(
            [self.buffer[2], self.buffer[3]]);

        // Update our running total.
        self.total_clk_cycles += clk_cycles as u64;
    }
}

impl Speed {
    pub fn mask(&self) -> u8 {
        use Speed::*;
        match self {
            Auto => 0b0001,
            Low  => 0b0010,
            Full => 0b0100,
            High => 0b1000,
        }
    }
}
