#[cfg(windows)]
#[allow(unsafe_code)] // Read-only Windows storage property queries on background workers.
mod windows {
    use std::fs::{File, OpenOptions};
    use std::mem::{offset_of, size_of};
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use std::ptr::{null, null_mut};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Storage::FileSystem::{
        BusTypeUsb, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_GET_HOTPLUG_INFO, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_DESCRIPTOR_HEADER, STORAGE_DEVICE_DESCRIPTOR, STORAGE_HOTPLUG_INFO,
        STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
    };

    const VALID_FOR: Duration = Duration::from_secs(10);

    #[derive(Clone, Copy, Default)]
    struct Slot {
        classified: Option<u8>,
        checked: Option<Instant>,
        running: bool,
        revision: u64,
    }

    static CACHE: Mutex<[Slot; 26]> = Mutex::new(
        [Slot {
            classified: None,
            checked: None,
            running: false,
            revision: 0,
        }; 26],
    );
    static REVISION: AtomicU64 = AtomicU64::new(0);

    pub fn tc_drive_revision() -> u64 {
        REVISION.load(Ordering::Acquire)
    }

    fn drive_index(path: &str) -> Option<usize> {
        let [letter, b':', b'/' | b'\\', ..] = path.as_bytes() else {
            return None;
        };
        let letter = letter.to_ascii_uppercase();
        letter
            .is_ascii_uppercase()
            .then(|| usize::from(letter - b'A'))
    }

    pub fn tc_drive_changed_since(path: &str, revision: u64) -> bool {
        drive_index(path).is_some_and(|index| {
            CACHE
                .lock()
                .is_ok_and(|cache| cache[index].revision > revision)
        })
    }

    pub fn request_tc_drive_type(path: &str) -> Option<u8> {
        let index = drive_index(path)?;
        let mut cache = CACHE.lock().ok()?;
        let slot = &mut cache[index];
        let current = slot.classified;
        if slot.running || slot.checked.is_some_and(|time| time.elapsed() < VALID_FOR) {
            return current;
        }
        slot.running = true;
        drop(cache);

        if std::thread::Builder::new()
            .name("arco-drive-type".into())
            .spawn(move || {
                let classified = probe(b'A' + index as u8);
                if let Ok(mut cache) = CACHE.lock() {
                    let slot = &mut cache[index];
                    if slot.classified != classified {
                        slot.classified = classified;
                        slot.revision = REVISION.fetch_add(1, Ordering::Release) + 1;
                    }
                    slot.running = false;
                    slot.checked = Some(Instant::now());
                }
            })
            .is_err()
            && let Ok(mut cache) = CACHE.lock()
        {
            let slot = &mut cache[index];
            slot.running = false;
            slot.checked = Some(Instant::now());
        }
        current
    }

    fn probe(letter: u8) -> Option<u8> {
        let root = [u16::from(letter), b':' as u16, b'\\' as u16, 0];
        // SAFETY: The drive root is NUL-terminated and lives through the call.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if !matches!(kind, 2 | 3) {
            return classify(kind, None, false);
        }
        let device = OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(format!(r"\\.\{}:", char::from(letter)))
            .ok();
        let storage = device.as_ref().and_then(descriptor);
        let hotplug = kind == 3
            && !storage.is_some_and(|(usb, _)| usb)
            && device.as_ref().is_some_and(device_hotplug);
        classify(kind, storage, hotplug)
    }

    fn classify(kind: u32, storage: Option<(bool, bool)>, hotplug: bool) -> Option<u8> {
        match (kind, storage, hotplug) {
            (2, Some((true, _)), _) | (3, Some((true, true)), _) => Some(2),
            (2, Some((false, _)), _) | (3, Some((true, false)), _) | (3, _, true) => Some(1),
            (3..=6, _, _) => Some(kind as u8),
            _ => None,
        }
    }

    fn descriptor(device: &File) -> Option<(bool, bool)> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut header = STORAGE_DESCRIPTOR_HEADER::default();
        let mut returned = 0;
        let input = (&raw const query).cast();
        let input_len = size_of::<STORAGE_PROPERTY_QUERY>() as u32;
        // SAFETY: Input and output buffers match the API's documented structures.
        let ok = unsafe {
            DeviceIoControl(
                device.as_raw_handle(),
                IOCTL_STORAGE_QUERY_PROPERTY,
                input,
                input_len,
                (&raw mut header).cast(),
                size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32,
                &mut returned,
                null_mut(),
            )
        };
        let bus_offset = offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType);
        let minimum = bus_offset + size_of::<i32>();
        let length = header.Size as usize;
        if ok == 0
            || returned < size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32
            || !(minimum..=65536).contains(&length)
        {
            return None;
        }
        let mut buffer = vec![0; length];
        // SAFETY: The second call receives a buffer sized from the validated descriptor header.
        let ok = unsafe {
            DeviceIoControl(
                device.as_raw_handle(),
                IOCTL_STORAGE_QUERY_PROPERTY,
                input,
                input_len,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 || (returned as usize) < minimum {
            return None;
        }
        let bus = i32::from_le_bytes(buffer[bus_offset..minimum].try_into().ok()?);
        let removable = buffer[offset_of!(STORAGE_DEVICE_DESCRIPTOR, RemovableMedia)] != 0;
        Some((bus == BusTypeUsb, removable))
    }

    fn device_hotplug(device: &File) -> bool {
        let mut info = [0u8; size_of::<STORAGE_HOTPLUG_INFO>()];
        let mut returned = 0;
        // SAFETY: The hotplug query has no input buffer and writes a fixed-size output structure.
        let ok = unsafe {
            DeviceIoControl(
                device.as_raw_handle(),
                IOCTL_STORAGE_GET_HOTPLUG_INFO,
                null(),
                0,
                info.as_mut_ptr().cast(),
                info.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        ok != 0
            && returned >= info.len() as u32
            && info[offset_of!(STORAGE_HOTPLUG_INFO, DeviceHotplug)] != 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn distinguishes_removable_usb_media_from_external_fixed_disks() {
            assert_eq!(classify(2, Some((true, true)), false), Some(2));
            assert_eq!(classify(3, Some((true, true)), false), Some(2));
            assert_eq!(classify(3, Some((true, false)), false), Some(1));
            assert_eq!(classify(3, Some((false, false)), true), Some(1));
            assert_eq!(classify(2, Some((false, true)), false), Some(1));
            assert_eq!(classify(2, None, false), None);
            assert_eq!(classify(3, None, false), Some(3));
            assert_eq!(classify(4, None, false), Some(4));
            assert_eq!(classify(5, None, false), Some(5));
            assert_eq!(classify(6, None, false), Some(6));
            assert_eq!(classify(1, None, false), None);
        }
    }
}

#[cfg(windows)]
pub use windows::{request_tc_drive_type, tc_drive_changed_since, tc_drive_revision};

#[cfg(not(windows))]
pub fn request_tc_drive_type(_: &str) -> Option<u8> {
    None
}

#[cfg(not(windows))]
pub fn tc_drive_revision() -> u64 {
    0
}

#[cfg(not(windows))]
pub fn tc_drive_changed_since(_: &str, _: u64) -> bool {
    false
}
