//! Types for representing non-packet events.
#![allow(dead_code)]

use crate::usb::Speed;

/// Types of events that may occur.
#[derive(Clone, Debug, PartialEq)]
pub enum EventType {
    /// Capture started with the specified speed.
    CaptureStart(Speed),
    /// Capture speed was changed.
    SpeedChange(Speed),
    /// Capture stopped for the specified reason.
    CaptureStop(StopReason),
    /// Line state change.
    LineStateChange(LineState),
    /// VBUS became invalid.
    VbusInvalid,
    /// VBUS became valid.
    VbusValid,
    /// Device attached at Low Speed.
    LsAttach,
    /// Device attached at Full Speed.
    FsAttach,
    /// Bus reset was detected.
    BusReset,
    /// Device HS chirp was validated.
    DeviceChirpValid,
    /// Host HS chirp was validated.
    HostChirpValid,
    /// Bus entered suspend.
    Suspend,
    /// Resume signal detected.
    Resume,
    /// Low Speed keepalive detected.
    LsKeepalive,
}

/// USB line states
#[derive(Clone, Debug, PartialEq)]
pub enum LineState {
    /// Single-ended 0: both D+ and D- at 0V.
    SE0,
    /// Chirp differential 1, i.e. J-state at ~0.6-0.8V.
    ChirpJ,
    /// Chirp differential 0, i.e. K-state at ~0.6-0.8V.
    ChirpK,
    /// Chirp single-ended 1, i.e. both D+ and D- at ~0.6-0.8V.
    ChirpSE1,
    /// LS J state, i.e. differential 0 at 3.3V while in LS mode.
    LsJ,
    /// LS K state, i.e. differential 1 at 3.3V while in LS mode.
    LsK,
    /// FS J state, i.e. differential 1 at 3.3V while in FS mode.
    FsJ,
    /// FS K state, i.e. differential 0 at 3.3V while in FS mode.
    FsK,
    /// Single-ended 1 at 3.3V.
    SE1,
}

/// A reason for stopping the capture.
#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    /// Requested by user.
    Requested,
    /// Capture buffer full.
    BufferFull,
    /// An error occured.
    Error,
}

use EventType::*;
use StopReason::*;
use Speed::*;
use LineState::*;

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            CaptureStop(Requested) => "Capture stopped by request",
            CaptureStop(BufferFull) => "Capture stopped due to full buffer",
            CaptureStop(Error) => "Capture stopped due to an error",
            CaptureStart(High) => "Capture started at High Speed (480 Mbps)",
            CaptureStart(Full) => "Capture started at Full Speed (12 Mbps)",
            CaptureStart(Low) => "Capture started at Low Speed (1.5 Mbps)",
            CaptureStart(Auto) => "Capture started with automatic speed selection",
            SpeedChange(High) => "Speed changed to High Speed (480 Mbps)",
            SpeedChange(Full) => "Speed changed to Full Speed (12 Mbps)",
            SpeedChange(Low) => "Speed changed to Low Speed (1.5 Mbps)",
            SpeedChange(Auto) => "Speed changed to automatic selection",
            LineStateChange(SE0)  => "SE0 line state detected",
            LineStateChange(ChirpJ) => "Chirp J state detected",
            LineStateChange(ChirpK) => "Chirp K state detected",
            LineStateChange(ChirpSE1) => "Chirp SE1 state detected",
            LineStateChange(LsJ) => "Low Speed idle state detected",
            LineStateChange(LsK) => "Low Speed resume state detected",
            LineStateChange(FsJ) => "Full Speed idle state detected",
            LineStateChange(FsK) => "Full Speed resume state detected",
            LineStateChange(SE1) => "Invalid SE1 line state detected",
            VbusInvalid => "VBUS voltage became invalid",
            VbusValid   => "VBUS voltage became valid",
            LsAttach => "Device attached at Low Speed",
            FsAttach => "Device attached at Full Speed",
            BusReset => "Bus reset",
            DeviceChirpValid => "Device HS chirp validated",
            HostChirpValid => "Host HS chirp validated",
            Suspend  => "Bus entered suspend",
            Resume   => "Resume signal detected",
            LsKeepalive => "Low Speed keepalive detected",
        })
    }
}

impl EventType {
    pub fn code(&self) -> u8 {
        match self {
            CaptureStop(Requested)    => 1,
            CaptureStop(BufferFull)   => 2,
            CaptureStop(Error)        => 3,
            CaptureStart(High)        => 4,
            CaptureStart(Full)        => 5,
            CaptureStart(Low)         => 6,
            CaptureStart(Auto)        => 7,
            SpeedChange(High)         => 8,
            SpeedChange(Full)         => 9,
            SpeedChange(Low)          => 10,
            SpeedChange(Auto)         => 11,
            LineStateChange(SE0)      => 12,
            LineStateChange(ChirpJ)   => 13,
            LineStateChange(ChirpK)   => 14,
            LineStateChange(ChirpSE1) => 15,
            LineStateChange(LsJ)      => 16,
            LineStateChange(LsK)      => 17,
            LineStateChange(FsJ)      => 18,
            LineStateChange(FsK)      => 19,
            LineStateChange(SE1)      => 20,
            VbusInvalid               => 21,
            VbusValid                 => 22,
            LsAttach                  => 23,
            FsAttach                  => 24,
            BusReset                  => 25,
            DeviceChirpValid          => 26,
            HostChirpValid            => 27,
            Suspend                   => 28,
            Resume                    => 29,
            LsKeepalive               => 30,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1  => CaptureStop(Requested),
            2  => CaptureStop(BufferFull),
            3  => CaptureStop(Error),
            4  => CaptureStart(High),
            5  => CaptureStart(Full),
            6  => CaptureStart(Low),
            7  => CaptureStart(Auto),
            8  => SpeedChange(High),
            9  => SpeedChange(Full),
            10 => SpeedChange(Low),
            11 => SpeedChange(Auto),
            12 => LineStateChange(SE0),
            13 => LineStateChange(ChirpJ),
            14 => LineStateChange(ChirpK),
            15 => LineStateChange(ChirpSE1),
            16 => LineStateChange(LsJ),
            17 => LineStateChange(LsK),
            18 => LineStateChange(FsJ),
            19 => LineStateChange(FsK),
            20 => LineStateChange(SE1),
            21 => VbusInvalid,
            22 => VbusValid,
            23 => LsAttach,
            24 => FsAttach,
            25 => BusReset,
            26 => DeviceChirpValid,
            27 => HostChirpValid,
            28 => Suspend,
            29 => Resume,
            30 => LsKeepalive,
            _  => return None
        })
    }
}
