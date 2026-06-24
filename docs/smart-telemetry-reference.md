# Windows SMART / Disk Telemetry – Developer Reference

A compact reference for expanding SMART and disk telemetry support in a Windows storage testing application (ATA + NVMe). Organized in the order you would typically implement the functionality.

---

# STEP 0 — Open the physical drive

Everything starts here.

```cpp
CreateFileW("\\\\.\\PhysicalDriveX")
```

Recommended flags:

```cpp
GENERIC_READ | GENERIC_WRITE
FILE_SHARE_READ | FILE_SHARE_WRITE
OPEN_EXISTING
```

You only need to do this once per drive.

---

# STEP 1 — Identify what kind of drive it is

This determines which SMART path you use (ATA vs NVMe).

## Call

```cpp
DeviceIoControl(
    IOCTL_STORAGE_QUERY_PROPERTY
)
```

## What to request

```cpp
StorageDeviceProperty
StorageAdapterProperty
```

## What you should extract

* Bus type (SATA / NVMe / USB / RAID)
* Model
* Serial number
* Firmware version
* Whether SMART is supported
* Whether it’s rotational or SSD

Once you have this, branch your code:

```cpp
if NVMe → Step 5
if SATA/ATA → Step 2
```

---

# STEP 2 — Read basic SMART status

## Call

```cpp
DeviceIoControl(
    SMART_RCV_DRIVE_DATA
)
```

## What to request

```cpp
SMART_READ_DATA
```

## What you get

* SMART enabled/disabled
* Failure prediction flag
* The 512-byte SMART data block (contains the full attribute table)

---

# STEP 3 — Read the full SMART attribute table

This is where real diagnostics start.

## Call

```cpp
DeviceIoControl(
    SMART_RCV_DRIVE_DATA
)
```

## What to request

```cpp
SMART_READ_DATA
```

## What to extract from the returned 512-byte buffer

For each attribute:

```text
Attribute ID
Current value
Worst value
Raw value (48-bit)
Flags
```

## The most useful attributes to expose

| ID      | Meaning                   |
| ------- | ------------------------- |
| 05      | Reallocated sectors       |
| C5      | Pending sectors           |
| C6      | Uncorrectable sectors     |
| 09      | Power-on hours            |
| 0C      | Power cycles              |
| C2      | Temperature (most drives) |
| E7 / E8 | SSD wear level            |
| F1 / F2 | Total writes (many SSDs)  |

---

# STEP 4 — Read SMART thresholds

This lets you show how close a drive is to failing.

## Call

```cpp
DeviceIoControl(
    SMART_RCV_DRIVE_DATA
)
```

## What to request

```cpp
SMART_READ_THRESHOLDS
```

## What you get

For each attribute:

```text
Threshold value
```

## What this enables in your UI

* Color-coded health
* "Approaching failure" warnings
* Accurate health scoring

---

# STEP 5 — NVMe SMART / Health information

Modern systems use NVMe — this is the most important upgrade if you don’t already support it.

## Call

```cpp
DeviceIoControl(
    IOCTL_STORAGE_PROTOCOL_COMMAND
)
```

## Protocol

```cpp
ProtocolTypeNvme
NVMe Log Page = SMART / Health Information (0x02)
```

## What you get

* Temperature
* Percentage used (SSD wear)
* Power-on hours
* Data units written
* Data units read
* Unsafe shutdown count
* Media errors
* Available spare %
* Critical warning flags

This is the NVMe equivalent of the SATA SMART table.

---

# STEP 6 — Optional (but extremely useful for a testing app)

These are not strictly SMART calls, but they make a storage testing application significantly more useful.

## Disk size / geometry

```cpp
IOCTL_DISK_GET_LENGTH_INFO
IOCTL_DISK_GET_DRIVE_GEOMETRY_EX
```

## Sector size / alignment

```cpp
IOCTL_STORAGE_QUERY_PROPERTY
(StorageAccessAlignmentProperty)
```

This lets you:

* Calculate test sizes correctly
* Avoid 4K alignment penalties
* Display useful hardware details in the UI

---

# Minimal implementation flow (complete overview)

```text
Open drive → CreateFileW

Identify drive → IOCTL_STORAGE_QUERY_PROPERTY

If SATA:
    SMART_RCV_DRIVE_DATA (SMART_READ_DATA)
    Parse attributes
    SMART_RCV_DRIVE_DATA (SMART_READ_THRESHOLDS)

If NVMe:
    IOCTL_STORAGE_PROTOCOL_COMMAND
    Read NVMe SMART / Health log

Optional:
    IOCTL_DISK_GET_LENGTH_INFO
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX
```

---

# Notes

This reference focuses only on:

* Built-in Windows APIs
* No third-party libraries
* Minimal but complete SMART + NVMe telemetry coverage

You can build a full-featured disk diagnostics tool using only the calls listed above.
