use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

use crate::coreaudio::{fourcc, read, read_ids, SYSTEM_OBJECT};

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

const PROCESS_LIST: u32 = fourcc(b"prs#");
const PID: u32 = fourcc(b"ppid");
const BUNDLE_ID: u32 = fourcc(b"pbid");
const RUNNING_INPUT: u32 = fourcc(b"piri");

/// Every process the HAL knows about, this one included.
pub fn processes() -> Result<Vec<AudioProcess>, HoldersError> {
    let objects = read_ids(SYSTEM_OBJECT, PROCESS_LIST).map_err(HoldersError::ProcessList)?;
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
