# Analisis: Apakah Modifikasi `.cil` Saja Sudah Cukup?

## Jawaban Singkat

**Tergantung "cukup" diartikan seperti apa:**

| Definisi "cukup" | Jawaban |
| --- | --- |
| Daemon rsc bisa jalan? | **YA** — `.cil` saja cukup, dengan syarat `.rc` pakai `seclabel` |
| Isolasi SELinux ketat? | **TIDAK** — butuh `file_contexts` juga untuk dedicated types |
| Mengikuti pattern Android standard? | **TIDAK** — vendor daemon pada umumnya butuh keduanya |
| Audit log avc denials bersih? | **TIDAK** — akan ada denials "broad" yang masih bisa diabaikan |

**Rekomendasi**: Edit **kedua file** (`vendor_sepolicy.cil` + `vendor_file_contexts`). Selisih effort hanya 1 file tambahan, tapi isolasi jauh lebih ketat.

## Penjelasan Teknis Lengkap

### Bagaimana SELinux Android 11 Melabeli File

Ada **2 mekanisme terpisah** untuk melabeli file di Android:

| Mekanisme | File | Scope | Contoh |
| --- | --- | --- | --- |
| **genfscon** (di CIL) | `vendor_sepolicy.cil` | Pseudo-filesystems (proc, sysfs, debugfs, debugrootfs) | `/proc/mtk_battery_cmd/*`, `/sys/devices/platform/battery/*` |
| **file_contexts** | `vendor_file_contexts` | On-disk files (ext4, f2fs) | `/vendor/bin/rsc`, `/data/adb/rsc/*` |

`genfscon` dan `file_contexts` **tidak saling menggantikan** — mereka melabeli jenis path yang berbeda. Tidak ada cara untuk melabeli `/vendor/bin/rsc` (file on-disk) lewat `genfscon`, dan sebaliknya tidak ada cara melabeli `/proc/mtk_battery_cmd` (pseudo-fs) lewat `file_contexts`.

### Path yang rsc Butuh

| Path | Jenis | Dilabeli oleh | Ada di CIL original? |
| --- | --- | --- | --- |
| `/proc/mtk_battery_cmd/en_power_path` | pseudo-fs (proc) | **genfscon** (CIL) | TIDAK — kita tambah |
| `/proc/mtk_battery_cmd/current_cmd` | pseudo-fs (proc) | **genfscon** (CIL) | TIDAK — kita tambah |
| `/sys/devices/platform/battery/disable_nafg` | pseudo-fs (sysfs) | **genfscon** (CIL) | SUDAH (line 345, type `sysfs_batteryinfo_30_0`) |
| `/sys/devices/platform/battery/ntc_disable_nafg` | pseudo-fs (sysfs) | **genfscon** (CIL) | SUDAH (prefix match) |
| `/sys/class/power_supply/battery/capacity` | pseudo-fs (sysfs) | **genfscon** (CIL) | SUDAH (via parent rule) |
| `/sys/class/power_supply/battery/status` | pseudo-fs (sysfs) | **genfscon** (CIL) | SUDAH (via parent rule) |
| `/vendor/bin/rsc` | on-disk (ext4) | **file_contexts** | TIDAK — kita tambah |
| `/data/adb/rsc/rsc.log` | on-disk (ext4) | **file_contexts** | TIDAK — kita tambah |

**Insight kunci**: dari 8 path yang rsc butuh, 6 di antaranya dilabeli via `genfscon` (CIL). Hanya 2 path on-disk yang butuh `file_contexts`.

### Skenario A: CIL Saja (Tanpa file_contexts)

Apa yang terjadi kalau kita edit `vendor_sepolicy.cil` saja?

#### Path /proc dan /sys — TIDAK ADA MASALAH

- `/proc/mtk_battery_cmd/*` → dilabeli `rsc_mtk_battery_proc` via genfscon baru di CIL ✓
- `/sys/devices/platform/battery/*` → tetap `sysfs_batteryinfo_30_0` (sudah ada) ✓
- `/sys/class/power_supply/battery/*` → tetap `sysfs_batteryinfo_30_0` ✓

Allow rules di CIL untuk type-type di atas → langsung jalan.

#### Binary /vendor/bin/rsc — MASALAH

Tanpa entry di `file_contexts`, binary jatuh ke default type:
```
/vendor/bin/rsc  →  u:object_r:vendor_file_30_0:s0   (default vendor file)
```

CIL rule yang kita tulis:
```cil
(typetransition init_30_0 rsc_exec process rsc)
```
**TIDAK FIRE** karena label binary `vendor_file_30_0` ≠ `rsc_exec`.

Akibatnya: init fork+exec rsc → process tetap di domain `init` (bukan `rsc`). Daemon jalan dengan privileges init — sangat broad, security risk.

#### Solusi: seclabel di .rc

Ada escape hatch. Di `rsc.rc`:
```
service rsc /vendor/bin/rsc
    seclabel u:r:rsc:s0
```

Directive `seclabel` memaksa init untuk:
1. `setexeccon(u:r:rsc:s0)` sebelum exec
2. Kernel melakukan domain transition berdasarkan seclabel, **bukan** berdasarkan label binary

Maka CIL cukup punya:
```cil
(allow init_30_0 rsc (process (transition)))
```

Dan daemon akan masuk domain `rsc` meski binary tetap `vendor_file_30_0`.

**Ini yang dilakukan varian `cil-only/` di kit ini.**

#### Directory /data/adb/rsc/ — MASALAH

Tanpa entry di `file_contexts`, directory baru yang rsc buat di `/data/adb/rsc/` mewarisi label parent:
```
/data/adb/rsc/  →  u:object_r:system_data_file_30_0:s0   (parent /data/adb label)
```

`system_data_file_30_0` adalah type yang dilabeli ke **semua** file di `/data/*` yang tidak punya label spesifik. Type ini **read-only untuk non-system domains** secara default.

Agar rsc bisa menulis log di sini, CIL harus allow:
```cil
(allow rsc system_data_file_30_0 (dir (create write ...)))
(allow rsc system_data_file_30_0 (file (create write ...)))
```

Ini **broad** — grant ini kasih rsc kemampuan menulis ke SEMUA file bertipe `system_data_file_30_0`, bukan hanya `/data/adb/rsc/`. Termasuk `/data/system/*`, `/data/data/*` (app data), dll.

**Security trade-off**: jika rsc punya bug (mis. path traversal), attacker bisa menulis ke file sistem mana pun. Tidak ideal, tapi acceptable untuk daemon root yang kecil dan sudah audited.

### Skenario B: CIL + file_contexts (Recommended)

Dengan `file_contexts` juga di-patch:

```file_contexts
/vendor/bin/rsc              u:object_r:rsc_exec:s0
/data/adb/rsc(/.*)?          u:object_r:rsc_data_file:s0
```

Sekarang:
- Binary → `rsc_exec` (dedicated) → `type_transition` fires → process masuk `rsc` domain otomatis
- Data dir → `rsc_data_file` (dedicated) → CIL allow hanya ke type ini, bukan ke `system_data_file_30_0` broadly

**Hasil**: isolasi ketat, audit log bersih (denials spesifik ke `rsc_data_file`, bukan broad `system_data_file`).

### Perbandingan Formal

| Aspek | CIL saja | CIL + file_contexts |
| --- | --- | --- |
| Daemon jalan | ✓ (dengan seclabel) | ✓ |
| Domain transition | Via seclabel (override) | Via type_transition (standard) |
| Binary label | `vendor_file_30_0` (default) | `rsc_exec` (dedicated) |
| Data dir label | `system_data_file_30_0` (broad) | `rsc_data_file` (dedicated) |
| Allow rule untuk data | `allow rsc system_data_file_30_0:file { write }` — BROAD | `allow rsc rsc_data_file:file { write }` — TIGHT |
| Risk jika bug di rsc | Bisa write ke semua `/data/*` | Hanya bisa write ke `/data/adb/rsc/*` |
| avc denial logs | Broad (susah trace) | Spesifik (mudah trace) |
| Standard Android pattern | Tidak (workaround) | Ya (match thermal_core, md_monitor, dll.) |
| File yang diedit | 1 (`vendor_sepolicy.cil`) | 2 (`vendor_sepolicy.cil` + `vendor_file_contexts`) |
| Selisih effort | — | +1 file edit |

### Mengapa Vendor Daemon pada Umumnya Pakai Keduanya

Lihat pattern vendor daemon yang sudah ada di device ini (dari CIL dump):

| Daemon | type_transition? | file_contexts entry? |
| --- | --- | --- |
| `thermal_core` | ✓ (line 8237) | ✓ (`/vendor/bin/thermal_core`) |
| `md_monitor` | ✓ | ✓ (`/vendor/bin/md_monitor`) |
| `fuelgauged` | ✓ | ✓ (`/vendor/bin/fuelgauged`) |
| `mtk_pkm_service` | ✓ | ✓ (`/vendor/bin/mtk_pkm_service`) |

**Tidak ada** vendor daemon di Infinix X695C yang menggunakan `seclabel` directive. Semuanya pakai `type_transition` + `file_contexts` pattern.

Alasannya bukan teknis (kedua approach jalan), tapi **convention**:
- `seclabel` dipakai biasanya untuk service yang butuh domain non-standard (mis. `su` domain)
- `type_transition` + `file_contexts` adalah cara "normal" untuk daemon vendor

### Tabel Keputusan

| Kondisi Anda | Pilih |
| --- | --- |
| Bisa edit 2 file via MT Manager | **CIL + file_contexts** (vendor-patched/) |
| Hanya bisa edit 1 file (e.g., MT Manager read-only untuk file_contexts) | **CIL only** (cil-only/) |
| Mau cara paling secure | **CIL + file_contexts** |
| Mau cara paling cepat | **CIL only** (1 file) |
| Ingin match pattern vendor lain di device | **CIL + file_contexts** |
| Khawatir breaking boot | **CIL only** dulu untuk test, lalu upgrade ke CIL + file_contexts |

### Catatan: file_contexts Tidak Selalu Bisa Di-Edit

Beberapa skenario di mana `file_contexts` sulit di-edit:

1. **dm-verity aktif** — `/vendor` read-only. Solusi: disable verity dulu.
2. **A/B partition dengan snapshot** — `/vendor` di-overlay. Solusi: edit di slot active.
3. **Vendor image signed** — modifikasi file_contexts bisa break signature check (tergantung AVB profile). Solusi: disable AVB verification.
4. **Magisk sudah modifikasi file_contexts** — bisa conflict. Solusi: jangan pakai approach ini, pakai Magisk module.

Di Infinix X695C dengan dm-verity disabled, semua skenario di atas tidak berlaku — `file_contexts` bisa di-edit bebas via MT Manager.

## Kesimpulan

**Untuk pertanyaan "apakah .cil saja cukup?":**

✅ **YA secara teknis** — daemon akan jalan dengan kombinasi:
1. CIL dengan allow rules untuk existing types (`sysfs_batteryinfo_30_0`, `system_data_file_30_0`)
2. `seclabel u:r:rsc:s0` di `.rc` untuk force domain transition
3. genfscon baru untuk `/proc/mtk_battery_cmd/*`

⚠️ **TIDAK secara best practice** — kehilangan:
1. Dedicated `rsc_exec` type → binary label broad
2. Dedicated `rsc_data_file` type → data write access broad
3. Pattern compliance dengan vendor daemon lain

**Saran**: kalau effort sama, pilih CIL + file_contexts (4 files di kit ini). Kalau ada batasan teknis yang menghalangi edit `file_contexts`, gunakan CIL only (1 file) — masih functional, hanya kurang ketat.
