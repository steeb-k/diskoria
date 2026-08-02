//! Linux block-device hotplug watcher — the `WM_DEVICECHANGE` counterpart.
//!
//! A `NETLINK_KOBJECT_UEVENT` socket (multicast group 1, the raw kernel
//! uevents — receivable without privileges) is watched from a dedicated
//! thread. Every `add`/`remove`/`change` event whose payload names
//! `SUBSYSTEM=block` sets the same `DEVICE_CHANGE_PENDING` flag the Windows
//! wndproc uses, then wakes the winit loop with a `Repaint` event so the
//! debounced re-enumeration in `about_to_wait` (800 ms, `lib.rs`) actually
//! runs — unlike Windows, nothing else would pump the loop awake here.

#![cfg(target_os = "linux")]

use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// Parse a raw kernel uevent datagram (`action@devpath\0KEY=VAL\0…`) and
/// decide whether it is a block-device add/remove/change.
pub(crate) fn is_block_device_event(datagram: &[u8]) -> bool {
    let mut parts = datagram.split(|b| *b == 0);
    let Some(header) = parts.next() else {
        return false;
    };
    let Ok(header) = std::str::from_utf8(header) else {
        return false;
    };
    let Some((action, _devpath)) = header.split_once('@') else {
        return false; // libudev-format packets (group 2) have no `@` header
    };
    if !matches!(action, "add" | "remove" | "change") {
        return false;
    }
    parts
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .any(|kv| kv == "SUBSYSTEM=block")
}

/// Spawn the watcher thread. Failure to open the netlink socket is logged and
/// tolerated — the Refresh button still works, only auto-refresh is lost.
pub fn spawn_uevent_watcher(proxy: EventLoopProxy<UserEvent>) {
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if fd < 0 {
        log::warn!(
            target: "diskoria",
            "uevent watcher: socket() failed ({}); drive hotplug auto-refresh disabled",
            std::io::Error::last_os_error()
        );
        return;
    }

    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_groups = 1; // kernel uevent multicast group
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        log::warn!(
            target: "diskoria",
            "uevent watcher: bind() failed ({}); drive hotplug auto-refresh disabled",
            std::io::Error::last_os_error()
        );
        unsafe { libc::close(fd) };
        return;
    }

    std::thread::Builder::new()
        .name("diskoria-uevent".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = unsafe {
                    libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
                };
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    log::warn!(target: "diskoria", "uevent watcher: recv failed ({err}); stopping");
                    break;
                }
                if n == 0 {
                    break;
                }
                if is_block_device_event(&buf[..n as usize]) {
                    crate::chrome::flag_device_change();
                    // Wake the loop; the 800 ms debounce in `about_to_wait`
                    // coalesces the burst a single hotplug produces.
                    if proxy
                        .send_event(UserEvent::Repaint {
                            after: std::time::Duration::ZERO,
                        })
                        .is_err()
                    {
                        break; // event loop gone
                    }
                }
            }
            unsafe { libc::close(fd) };
        })
        .expect("spawn uevent watcher");
}

#[cfg(test)]
mod tests {
    use super::is_block_device_event;

    fn dg(parts: &[&str]) -> Vec<u8> {
        parts.join("\0").into_bytes()
    }

    #[test]
    fn block_add_is_detected() {
        let d = dg(&[
            "add@/devices/pci0000:00/usb2/2-1/host6/target6:0:0/6:0:0:0/block/sdb",
            "ACTION=add",
            "DEVPATH=/devices/…/block/sdb",
            "SUBSYSTEM=block",
            "DEVNAME=sdb",
        ]);
        assert!(is_block_device_event(&d));
    }

    #[test]
    fn non_block_subsystem_is_ignored() {
        let d = dg(&["add@/devices/usb2/2-1", "ACTION=add", "SUBSYSTEM=usb"]);
        assert!(!is_block_device_event(&d));
    }

    #[test]
    fn libudev_packets_without_header_are_ignored() {
        // udevd's group-2 packets start with "libudev" and a binary header.
        assert!(!is_block_device_event(b"libudev\x00\xfe\xed\xca\xfe"));
    }

    #[test]
    fn bind_and_unbind_actions_are_ignored() {
        let d = dg(&["bind@/devices/x", "ACTION=bind", "SUBSYSTEM=block"]);
        assert!(!is_block_device_event(&d));
    }
}
