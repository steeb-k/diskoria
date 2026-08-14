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

# STEP 5b — USB enclosures: ATA over SCSI pass-through (SAT)

`SMART_RCV_DRIVE_DATA` is not implemented by the USBSTOR/UASP stack, so the
STEP 3 path returns nothing for an external drive. Most bridges *do* implement
SAT (SCSI/ATA Translation), which forwards an ATA command descriptor block to
the drive unchanged.

## Call

```cpp
DeviceIoControl(
    IOCTL_SCSI_PASS_THROUGH_DIRECT      // 0x0004D014
)
```

`SCSI_PASS_THROUGH_DIRECT` is not in `windows-sys`; Diskoria declares it in
`smart_reader/windows.rs`. Allocate the header and its sense buffer as one
block and set `SenseInfoOffset` to `sizeof(SCSI_PASS_THROUGH_DIRECT)`. The
handle must be `GENERIC_READ | GENERIC_WRITE` (so: elevated).

## The CDB

ATA PASS-THROUGH (16), opcode `0x85`. Retry with ATA PASS-THROUGH (12), opcode
`0xA1`, for older bridges that only implement the short form — the register
fields sit at different offsets, which is why the two CDB builders are separate
functions in `smart_reader/mod.rs`.

```
byte 0  0x85              opcode
byte 1  0x08              protocol 4 (PIO Data-In) << 1
byte 2  0x0E              T_DIR = from device, BYT_BLOK = blocks,
                          T_LENGTH = sector count field
byte 6  0x01              SECTOR_COUNT
byte 10 0x4F / byte 12 0xC2   LBA_MID/LBA_HIGH — the SMART magic
                              (zero for IDENTIFY DEVICE)
byte 13 0xA0              DEVICE
byte 14 0xB0 / 0xEC       COMMAND: SMART / IDENTIFY DEVICE
```

## What you get

* `0xB0` with FEATURES `0xD0`/`0xD1` returns the same 512-byte SMART attribute
  and threshold payloads as STEP 3, so the STEP 3 parser is reused verbatim.
* `0xEC` (IDENTIFY DEVICE) returns the drive's real identity — the model string
  at words 27..46 is the *drive*, not the enclosure's marketing name — and
  **word 217, nominal media rotation rate**: `1` = solid state, `0x0401..0xFFFE`
  = RPM, `0` = not reported. This is the only reliable way to tell a spinning
  disk from an SSD inside a USB enclosure; `MSFT_PhysicalDisk.MediaType` reports
  Unspecified and the seek-penalty query in STEP 1 is rejected outright. See
  known-issues KI-57.

USB-NVMe enclosures speak a vendor-specific protocol instead and will fail
every CDB above; treat that as "no data", not as an error state.

## Asking whether the drive is awake (`CK_COND`)

Every command above touches the medium and will spin a sleeping disk back up.
**CHECK POWER MODE (`0xE5`)** will not — it is answered by the drive's
electronics — so it goes first, and a drive in standby is left alone
(known-issues KI-58; the same rule as `smartctl -n standby`).

It is a *non-data* command, so its answer arrives in a register rather than a
payload:

```
byte 1  0x06              protocol 3 (Non-data) << 1
byte 2  0x20              CK_COND set, T_LENGTH = 0
byte 14 0xE5              COMMAND: CHECK POWER MODE
```

`CK_COND` tells the translator to return the ATA output registers as sense data.
The command then reports **CHECK CONDITION** with sense key RECOVERED ERROR and
ASC/ASCQ `00/1D` *on success*, so a non-zero SCSI status here is the expected
outcome — a data-in helper that treats it as failure will throw the answer away.
Set `DataIn = SCSI_IOCTL_DATA_UNSPECIFIED` with a null buffer on Windows, or
`SG_DXFER_NONE` on Linux.

The registers come back in descriptor-format sense (SPC-4 §4.5.2) as an **ATA
Status Return descriptor**:

```
byte 0  0x72 / 0x73       descriptor-format sense
byte 7  additional length; descriptors start at byte 8
  desc byte 0  0x09       ATA Status Return descriptor
  desc byte 1  0x0C       descriptor length
  desc byte 5  ...        SECTOR_COUNT (7:0)  <- the power mode
```

`SECTOR_COUNT`: `0x00` standby, `0x40` NV cache with the spindle stopped,
`0x80..0x83` idle, `0x41`/`0xFF` active. Anything else — or fixed-format sense,
which carries no register block — means "no answer": poll anyway rather than
mistake silence for sleep.

The Linux equivalent is `SG_IO` with the identical 16-byte CDB — same bytes,
different transport — which is why the CDB builders are shared pure logic in
`smart_reader/mod.rs` rather than living in either platform file.

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
