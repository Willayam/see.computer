use std::ffi::c_void;

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

/// A process other than this one that the HAL reports as running audio input now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicHolder {
    pub pid: i32,
    pub bundle_id: Option<String>,
}

/// A HAL process object: the id a process tap names, and what it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioProcess {
    pub object: u32,
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub running_input: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HoldersError {
    #[error("CoreAudio process list failed with OSStatus {0}")]
    ProcessList(i32),
}

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

const fn fourcc(bytes: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*bytes)
}

const PROCESS_LIST: u32 = fourcc(b"prs#");
const PID: u32 = fourcc(b"ppid");
const BUNDLE_ID: u32 = fourcc(b"pbid");
const RUNNING_INPUT: u32 = fourcc(b"piri");
const GLOBAL: u32 = fourcc(b"glob");
const SYSTEM_OBJECT: u32 = 1;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

fn address(selector: u32) -> Address {
    Address {
        selector,
        scope: GLOBAL,
        element: 0,
    }
}

fn read<T: Copy>(object: u32, selector: u32) -> Result<T, i32> {
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address(selector),
            0,
            std::ptr::null(),
            &mut size,
            value.as_mut_ptr().cast(),
        )
    };
    if status == 0 && size == std::mem::size_of::<T>() as u32 {
        Ok(unsafe { value.assume_init() })
    } else {
        Err(status)
    }
}

/// Every process the HAL knows about, this one included.
pub fn processes() -> Result<Vec<AudioProcess>, HoldersError> {
    let mut size = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            SYSTEM_OBJECT,
            &address(PROCESS_LIST),
            0,
            std::ptr::null(),
            &mut size,
        )
    };
    if status != 0 {
        return Err(HoldersError::ProcessList(status));
    }
    let count = size as usize / std::mem::size_of::<u32>();
    let mut objects = vec![0u32; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address(PROCESS_LIST),
            0,
            std::ptr::null(),
            &mut size,
            objects.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Err(HoldersError::ProcessList(status));
    }
    objects.truncate(size as usize / std::mem::size_of::<u32>());
    Ok(objects
        .into_iter()
        .filter_map(|object| {
            let pid = read::<i32>(object, PID).ok()?;
            let running_input = read::<u32>(object, RUNNING_INPUT).ok()? != 0;
            let bundle_id = read::<CFStringRef>(object, BUNDLE_ID).ok().and_then(|raw| {
                if raw.is_null() {
                    return None;
                }
                let string = unsafe { CFString::wrap_under_create_rule(raw) }.to_string();
                (!string.is_empty()).then_some(string)
            });
            Some(AudioProcess {
                object,
                pid,
                bundle_id,
                running_input,
            })
        })
        .collect())
}

pub fn mic_holders() -> Result<Vec<MicHolder>, HoldersError> {
    let own = std::process::id() as i32;
    Ok(processes()?
        .into_iter()
        .filter(|process| process.running_input && process.pid != own)
        .map(|process| MicHolder {
            pid: process.pid,
            bundle_id: process.bundle_id,
        })
        .collect())
}
