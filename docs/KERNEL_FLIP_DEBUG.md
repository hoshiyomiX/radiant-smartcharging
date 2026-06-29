# Debugging Guide — Kernel Flip (Instant Charge Status Bounce)

## Symptom

The daemon log shows `charging=true → charging=false → charging=true` transitions
within **the same second** (0-second gap). Physically impossible to unplug + replug
a charger that fast — this is a kernel/driver-level state flip, not a real plug
event.

### Example from v0.0.4 log

```
13:51:55  seq=70  cap=72%, Discharging, charging=false, thermal_on=true
13:51:55  seq=71  thermal delimiter DISABLED (reason=charger_unplugged)
13:51:55  seq=72  cap=72%, Charging, charging=true, thermal_on=false
13:51:55  seq=73  thermal delimiter ENABLED (reason=charging_detected)
```

All four lines share the same timestamp `13:51:55`. Compare with a real
intentional unplug+replug (10-second gap):

```
12:57:39  seq=27  cap=33%, Discharging, charging=false
12:57:39  seq=28  thermal delimiter DISABLED
12:57:49  seq=29  cap=33%, Charging, charging=true   ← 10s later
12:57:49  seq=30  thermal delimiter ENABLED
```

## Root cause hypotheses (most likely first)

1. **USB PD renegotiation** — charger and device negotiate a new power profile
   (e.g. switch from 5V/3A to 9V/2A), and the MTK charger driver briefly
   reports `Discharging` during the transition.
2. **MTK battery driver bug** — emits a spurious `Discharging` uevent during
   fast-charge current limit changes or fuel-gauge recalibration.
3. **Loose USB-C cable / dirty port** — high-resistance connection causes the
   driver to detect a brief disconnect when current spikes.
4. **Charger firmware glitch** — cheap chargers may drop negotiation briefly
   under load.

## Step-by-step debugging

### Step 1 — Confirm the pattern

Check the daemon log for the 0-second-gap signature:

```bash
adb shell cat /data/adb/rsc/rsc.log | grep -E "event=state|thermal delimiter"
```

Look for consecutive `state` lines where `charging` flips `true → false → true`
within the same second. Count how often this happens — if it's once per
session, it's likely PD renegotiation. If it's frequent (>5x/hour), suspect
hardware.

### Step 2 — Capture the raw uevent stream

The daemon filters uevents in user space. To see what the kernel actually
emits, run `udevadm`-equivalent on Android via `adb shell`:

```bash
# Method A: cat the netlink socket via simple Python listener
adb shell su -c 'python3 -c "
import socket
s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 15)
s.bind((0, 1))
while True:
    data, _ = s.recvfrom(8192)
    text = data.decode(errors=\"replace\")
    if \"power_supply\" in text:
        print(text.replace(chr(0), \" | \"), flush=True)
"'

# Method B: monitor specific battery sysfs attributes
adb shell su -c 'while true; do
  echo \"$(date +%H:%M:%S.%N) status=$(cat /sys/class/power_supply/battery/status) current=$(cat /sys/class/power_supply/battery/current_now) voltage=$(cat /sys/class/power_supply/battery/voltage_now)\"
  sleep 0.2
done'
```

Reproduce the kernel flip — plug/unplug the charger a few times. Look for
`status=Discharging` flashes that last <1 second.

### Step 3 — Check USB PD negotiation logs

```bash
# TCPM (Type-C Port Manager) logs — shows PD contract negotiation
adb shell su -c 'cat /sys/kernel/debug/tcpm/*/log 2>/dev/null | tail -100'

# Or dump recent kernel log for USB/PD messages
adb shell su -c 'dmesg | grep -iE "usb|pd|tcpm|charger|mtk_battery|mt-power" | tail -100'
```

Look for messages around the timestamp of the kernel flip:
- `PD contract changed` / `PD source cap` → USB PD renegotiation
- `mtk-charger: ...` → MTK charger driver events
- `over-current` / `vbus drop` → cable/port issue

### Step 4 — Check MTK battery driver logs

```bash
# MTK battery driver debug nodes (varies by BSP)
adb shell su -c 'ls /sys/class/power_supply/battery/'
adb shell su -c 'ls /proc/mtk_battery_cmd/'
adb shell su -c 'ls /sys/devices/platform/battery/'

# Watch current_now + voltage_now at high frequency during flip
adb shell su -c 'while true; do
  echo \"$(date +%H:%M:%S.%N) current=$(cat /sys/class/power_supply/battery/current_now) voltage=$(cat /sys/class/power_supply/battery/voltage_now) temp=$(cat /sys/class/power_supply/battery/temp)\"
  sleep 0.1
done' | tee current-log.txt
```

If `current_now` drops to ~0 or goes negative briefly, then recovers —
this confirms the FET/charger path actually interrupted, even if the
cable stayed plugged in.

### Step 5 — Isolate hardware vs software

Run these tests in order:

| Test | Expected if hardware issue | Expected if software/driver issue |
|------|---------------------------|-----------------------------------|
| **Different charger, same cable** | Flip stops | Flip continues |
| **Same charger, different cable** | Flip stops | Flip continues |
| **Same charger+cable, clean port** | Flip stops | Flip continues |
| **Different charger + different cable** | Flip stops | Flip continues |
| **Device powered off, charge 1h, reboot** | Boot shows correct cap | Cap jumps/drops weirdly |

If the flip stops with a different charger/cable → **hardware**. Replace
the offending component.

If the flip continues regardless of hardware → **MTK driver bug**. No
fix at user level — the daemon's debounce mitigation (see below) is the
practical workaround.

### Step 6 — Check charger type detection

```bash
adb shell su -c 'cat /sys/class/power_supply/usb/real_type 2>/dev/null'
adb shell su -c 'cat /sys/class/power_supply/usb/type 2>/dev/null'
adb shell su -c 'cat /sys/class/power_supply/charger/type 2>/dev/null'
```

Some MTK BSPs renegotiate PD contract periodically (especially with
non-standard chargers like car chargers or power banks). If `real_type`
changes between reads (e.g. `SDP → DCP → SDP`), that's the source of
the flip.

### Step 7 — Long-term monitoring

Set up a 24-hour capture to see if the flip is periodic (suggests PD
renegotiation timer) or random (suggests hardware):

```bash
adb shell su -c 'nohup sh -c "
while true; do
  echo \"$(date +%Y-%m-%dT%H:%M:%S) status=$(cat /sys/class/power_supply/battery/status) current=$(cat /sys/class/power_supply/battery/current_now)\"
  sleep 5
done
" > /data/adb/rsc/flip-monitor.log 2>&1 &'
```

After 24h, analyze:

```bash
adb shell cat /data/adb/rsc/flip-monitor.log | \
  awk -F'status=' '{print $2}' | \
  awk '{print $1}' | \
  uniq -c
```

A healthy setup shows 2-3 status changes per day (plug/unplug events).
10+ changes per hour without physical plug events = problem.

## Code-level mitigation (already implemented in v1.0.1+)

The daemon now includes a **debounce** in `tick()` that suppresses
thermal delimiter toggles when the charging status flips within a
2-second window. Look for `CHARGE_FLIP_DEBOUNCE_MS` in `src/main.rs`.

This means:

- **Real plug/unplug** (gap > 2s) → thermal toggles normally, full
  event logged.
- **Kernel flip** (gap < 2s) → thermal toggle suppressed, only a
  debug-level log line `event=charge_flip_suppressed` is emitted.

The daemon stays correct — thermal delimiter state is preserved
(whatever it was before the flip, it stays). No sysfs writes wasted
on phantom events.

## What to report if you file an issue

If debugging confirms it's an MTK driver bug and you want to report
upstream (or to Transsion), include:

1. Daemon log excerpt showing the 0-second flip
2. `dmesg` excerpt around the same timestamp (filter `mtk` / `charger`)
3. `tcpm` log excerpt (if PD renegotiation is suspected)
4. `current_now` + `voltage_now` high-frequency capture during the flip
5. Charger model + cable type + device firmware version
6. Whether the flip stops with a different charger/cable (Step 5 results)

The more data points, the easier it is for the BSP team to reproduce.
