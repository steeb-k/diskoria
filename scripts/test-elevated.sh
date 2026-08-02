#!/usr/bin/env bash
# Elevated (root) verification of the Linux storage backend. Run as:
#
#     sudo ./scripts/test-elevated.sh
#
# What it does — nothing here touches a real disk destructively:
#   1. builds unprivileged (as $SUDO_USER, so target/ stays user-owned)
#   2. elevated drive enumeration (real partition-table styles, LUKS states)
#   3. real SMART/NVMe readout via the ioctls + smartctl cross-check
#   4. hwmon temperature fallback
#   5. surface scan AND destructive write+verify against a disposable
#      256 MB file-backed loop device (created, formatted, mounted — so the
#      unmount pre-flight is exercised — then detached and deleted)
#   6. a short elevated GUI smoke on the invoking user's display, checking
#      the app's own SMART queries succeed as root
set -uo pipefail

if [[ ${EUID} -ne 0 ]]; then
    echo "Run with: sudo $0" >&2
    exit 1
fi

invoker="${SUDO_USER:-root}"
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root/diskoria"

run_as_user() {
    if [[ "$invoker" != "root" ]]; then
        sudo -u "$invoker" env PATH="$PATH" "$@"
    else
        "$@"
    fi
}

fail=0
section() { echo; echo "════ $* ════"; }
result()  { if [[ $1 -eq 0 ]]; then echo "── OK: $2"; else echo "── FAIL: $2"; fail=1; fi }

section "0. build (as $invoker)"
run_as_user cargo test --no-run --quiet || { echo "build failed"; exit 1; }
run_as_user cargo build --quiet || { echo "build failed"; exit 1; }

# The lib unit-test binary (the ignored diagnostics live there). Run it as
# root directly so cargo never executes as root (keeps target/ user-owned).
TEST_BIN=$(run_as_user cargo test --no-run --message-format=json 2>/dev/null \
    | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        j = json.loads(line)
    except ValueError:
        continue
    t = j.get("target", {})
    if t.get("name") == "diskoria" and "lib" in t.get("kind", []) and j.get("executable"):
        print(j["executable"])
')
[[ -n "$TEST_BIN" ]] || { echo "could not locate the test binary"; exit 1; }
echo "test binary: $TEST_BIN"

section "1. elevated drive enumeration (styles should be Gpt/Mbr, not Unknown)"
"$TEST_BIN" --ignored --nocapture --exact drive_enumeration::linux::hardware_tests::print_real_enumeration
result $? "enumeration"

section "2. real SMART readout (compare with smartctl below)"
"$TEST_BIN" --ignored --nocapture --exact smart_reader::hardware_tests::print_real_smart_reports
result $? "SMART ioctls"

if command -v smartctl >/dev/null; then
    for dev in /dev/nvme?n1 /dev/sd?; do
        [[ -e "$dev" ]] || continue
        echo "--- smartctl -A $dev (reference) ---"
        smartctl -A "$dev" | sed -n '4,40p'
    done
else
    echo "(smartctl not installed — skipping cross-check)"
fi

section "3. hwmon temperatures"
"$TEST_BIN" --ignored --nocapture --exact monitor::hwmon_tests::print_hwmon_temps
result $? "hwmon"

section "4. loop-device surface scan + destructive write+verify"
scratch="$(mktemp -d)"
img="$scratch/diskoria-loop.img"
mnt="$scratch/mnt"
loopdev=""
cleanup() {
    mountpoint -q "$mnt" 2>/dev/null && umount "$mnt"
    [[ -n "$loopdev" ]] && losetup -d "$loopdev" 2>/dev/null
    rm -rf "$scratch"
}
trap cleanup EXIT

dd if=/dev/zero of="$img" bs=1M count=256 status=none
loopdev="$(losetup --find --show "$img")"
# Safety: the disk tests must only ever see the throwaway loop device. The
# destructive test additionally refuses non-loop devices on its own (see
# destructive_test::device_tests::assert_loop_device), but belt and braces.
case "$loopdev" in
    /dev/loop*) ;;
    *) echo "── FAIL: losetup returned unexpected device '$loopdev'; aborting disk tests"; exit 1 ;;
esac
if [[ "$(losetup -O BACK-FILE --noheadings "$loopdev" | xargs)" != "$img" ]]; then
    echo "── FAIL: $loopdev is not backed by $img; aborting disk tests"
    exit 1
fi
echo "loop device: $loopdev (256 MB, backed by $img — real drives are only read)"

DISKORIA_TEST_DEVICE="$loopdev" \
    "$TEST_BIN" --ignored --nocapture --exact surface_test::device_tests::scan_device_from_env
result $? "surface scan"

# Format + mount so the destructive pre-flight has something to unmount.
mkdir -p "$mnt"
if mkfs.ext4 -q "$loopdev" && mount "$loopdev" "$mnt"; then
    echo "formatted ext4 and mounted at $mnt — the test must unmount it itself"
    DISKORIA_TEST_DEVICE="$loopdev" DISKORIA_TEST_MOUNTS="$mnt" \
        "$TEST_BIN" --ignored --nocapture --exact destructive_test::device_tests::write_verify_device_from_env
    result $? "destructive write+verify (with unmount pre-flight)"
    mountpoint -q "$mnt" && { echo "── FAIL: device is still mounted"; fail=1; } \
        || echo "── OK: device was unmounted by the pre-flight"
else
    echo "(mkfs.ext4/mount unavailable — running destructive test unmounted)"
    DISKORIA_TEST_DEVICE="$loopdev" \
        "$TEST_BIN" --ignored --nocapture --exact destructive_test::device_tests::write_verify_device_from_env
    result $? "destructive write+verify"
fi

section "5. elevated GUI smoke (real SMART through the app)"
user_uid="$(id -u "$invoker")"
runtime_dir="/run/user/$user_uid"
wayland_sock="$(ls "$runtime_dir"/wayland-* 2>/dev/null | head -1 | xargs -r basename)"
gui_log="$scratch/gui.log"
env XDG_RUNTIME_DIR="$runtime_dir" \
    ${wayland_sock:+WAYLAND_DISPLAY="$wayland_sock"} \
    DISPLAY="${DISPLAY:-:0}" \
    HOME="$(getent passwd "$invoker" | cut -d: -f6)" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime_dir/bus" \
    DISKORIA_SMOKE=6 RUST_LOG=diskoria=info \
    ./target/debug/diskoria >"$gui_log" 2>&1
gui_rc=$?
grep -E "query_(ata|nvme|ufs): OK|poll_monitor|Monitor thread" "$gui_log" | head -6
result $gui_rc "GUI smoke (exit $gui_rc)"
grep -q "query_.*: OK" "$gui_log" \
    && echo "── OK: app read real SMART data while elevated" \
    || { echo "── FAIL: no successful SMART query in the app log"; fail=1; }

section "summary"
if [[ $fail -eq 0 ]]; then
    echo "ALL ELEVATED CHECKS PASSED"
else
    echo "SOME CHECKS FAILED — see sections above"
fi
exit $fail
