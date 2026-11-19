//! Raw reads of the few HAL properties that cpal does not expose.

use std::ffi::c_void;

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

pub const fn fourcc(bytes: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*bytes)
}

pub const SYSTEM_OBJECT: u32 = 1;
const GLOBAL: u32 = fourcc(b"glob");

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

/// A fixed-size property, or the OSStatus that refused it.
pub fn read<T: Copy>(object: u32, selector: u32) -> Result<T, i32> {
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

/// A property holding a list of object ids, or the OSStatus that refused it.
pub fn read_ids(object: u32, selector: u32) -> Result<Vec<u32>, i32> {
    let mut size = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(object, &address(selector), 0, std::ptr::null(), &mut size)
    };
    if status != 0 {
        return Err(status);
    }
    let mut ids = vec![0u32; size as usize / std::mem::size_of::<u32>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address(selector),
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    ids.truncate(size as usize / std::mem::size_of::<u32>());
    Ok(ids)
}
