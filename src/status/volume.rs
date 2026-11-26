//! Master volume of the default playback device, via Core Audio.
//!
//! Two COM callbacks run on audio worker threads. Volume changes carry the new
//! level and mute flag, which are stored in atomics, so the UI thread never calls
//! into the audio service just to repaint. Default-device changes make the UI
//! thread re-bind to the new device.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{PCWSTR, Result, implement};

use super::{SOURCE_AUDIO_DEVICE, SOURCE_VOLUME, notify};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VolumeState {
    /// 0.0 ..= 1.0
    pub level: f32,
    pub muted: bool,
}

static AVAILABLE: AtomicBool = AtomicBool::new(false);
static LEVEL_BITS: AtomicU32 = AtomicU32::new(0);
static MUTED: AtomicBool = AtomicBool::new(false);

fn store(state: Option<VolumeState>) {
    if let Some(state) = state {
        LEVEL_BITS.store(state.level.to_bits(), Ordering::SeqCst);
        MUTED.store(state.muted, Ordering::SeqCst);
    }
    AVAILABLE.store(state.is_some(), Ordering::SeqCst);
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeChanged;

impl IAudioEndpointVolumeCallback_Impl for VolumeChanged_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if let Some(data) = unsafe { data.as_ref() } {
            store(Some(VolumeState {
                level: data.fMasterVolume,
                muted: data.bMuted.as_bool(),
            }));
            notify(SOURCE_VOLUME);
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceChanged;

impl IMMNotificationClient_Impl for DeviceChanged_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            notify(SOURCE_AUDIO_DEVICE);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

pub struct Volume {
    enumerator: IMMDeviceEnumerator,
    device_callback: IMMNotificationClient,
    volume_callback: IAudioEndpointVolumeCallback,
    /// `None` when there is no playback device at all.
    endpoint: Option<IAudioEndpointVolume>,
}

impl Volume {
    pub fn new() -> Result<Self> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let device_callback: IMMNotificationClient = DeviceChanged.into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&device_callback)? };
        let mut volume = Self {
            enumerator,
            device_callback,
            volume_callback: VolumeChanged.into(),
            endpoint: None,
        };
        volume.rebind();
        Ok(volume)
    }

    /// (Re)attaches to the current default playback device.
    pub fn rebind(&mut self) {
        if let Some(old) = self.endpoint.take() {
            unsafe {
                let _ = old.UnregisterControlChangeNotify(&self.volume_callback);
            }
        }
        self.endpoint = unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|device| device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None))
                .ok()
        };
        if let Some(endpoint) = &self.endpoint {
            unsafe {
                let _ = endpoint.RegisterControlChangeNotify(&self.volume_callback);
            }
        }
        store(self.read_endpoint());
    }

    fn read_endpoint(&self) -> Option<VolumeState> {
        let endpoint = self.endpoint.as_ref()?;
        unsafe {
            Some(VolumeState {
                level: endpoint.GetMasterVolumeLevelScalar().ok()?,
                muted: endpoint.GetMute().ok()?.as_bool(),
            })
        }
    }
}

/// Last known volume (cheap, any thread).
pub fn current() -> Option<VolumeState> {
    AVAILABLE.load(Ordering::SeqCst).then(|| VolumeState {
        level: f32::from_bits(LEVEL_BITS.load(Ordering::SeqCst)),
        muted: MUTED.load(Ordering::SeqCst),
    })
}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            if let Some(endpoint) = &self.endpoint {
                let _ = endpoint.UnregisterControlChangeNotify(&self.volume_callback);
            }
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.device_callback);
        }
    }
}
