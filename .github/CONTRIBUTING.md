# Contributing to rsc

Thanks for your interest in contributing! This document covers the
basics. For complex changes, please open an issue first to discuss.

## Development environment

- Rust stable (>= 1.74) — https://rustup.rs
- Android NDK r25+ (for cross-compile testing)
- Python 3 (for `selinux/check_cil.py` syntax validator)
- An MTK Android device with root access (for runtime testing)

## Workflow

1. **Fork & clone** the repo.
2. **Create a branch** for your feature/fix:
   ```bash
   git checkout -b feat/my-feature
   # or: fix/issue-123, docs/update-readme, etc.
   ```
3. **Make changes**. Keep commits focused — one logical change per commit.
4. **Run checks** before pushing:
   ```bash
   cargo check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
5. **Test on device** if your change affects runtime behavior. See
   `docs/AUDIT.md` for the debug workflow.
6. **Update CHANGELOG.md** under `[Unreleased]` — add a bullet under
   Added/Changed/Fixed/Removed as appropriate.
7. **Commit** with conventional commit format (see below).
8. **Push** and open a Pull Request.

## Conventional commits

We use [Conventional Commits](https://www.conventionalcommits.org/) for
clear history and automatic changelog generation:

```
<type>(<scope>): <subject>

<body>

<footer>
```

Types:
- `feat` — new feature
- `fix` — bug fix
- `docs` — documentation only
- `style` — formatting, no code change
- `refactor` — code change that neither fixes a bug nor adds a feature
- `perf` — code change that improves performance
- `test` — adding or correcting tests
- `chore` — build, deps, config, etc.

Scopes (optional but encouraged):
- `uevent` — uevent listener / parser
- `mtk` — MTK sysfs/proc interface
- `config` — config loading
- `logger` — logging
- `selinux` — CIL policy / file_contexts
- `android` — .rc, build.sh, install.sh
- `docs` — README, AUDIT, etc.

Examples:
```
feat(uevent): parse POWER_SUPPLY_CAPACITY from payload
fix(mtk): handle EBUSY on current_cmd write
docs(selinux): add troubleshooting for neverallow violations
perf(uevent): drain queued events in single pass
```

## Code style

- Run `cargo fmt` before committing.
- Follow clippy suggestions (treat warnings as errors in CI).
- Public functions need doc comments (`///`).
- Unsafe blocks need a `// SAFETY:` comment explaining the invariant.
- No `unwrap()`/`expect()` in production code paths — handle errors
  explicitly. Tests can use them freely.

## Testing

- Unit tests live in `#[cfg(test)] mod tests` blocks within each module.
- Integration tests live in `tests/`.
- Run all tests: `cargo test`.
- For device-specific changes, test on actual hardware and report
  results in the PR description.

## SELinux policy changes

If your change modifies `selinux/rsc.cil`:

1. Run the syntax validator:
   ```bash
   python3 selinux/check_cil.py
   ```
2. Rebuild the pre-patched CIL:
   ```bash
   cat <original vendor_sepolicy.cil> selinux/rsc.cil > selinux/vendor_sepolicy.cil.patched
   ```
3. Document the change in `selinux/README.md` if it affects
   compatibility (new neverallow, new type, etc.).
4. Update `CHANGELOG.md`.

## Compatibility changes

If your change affects compatibility (new sysfs path, new SELinux
type, new config field), follow SemVer:

- **Major**: breaking change (e.g. v0.3.0 removed polling fallback —
  users with broken SELinux policy must fix it before upgrade)
- **Minor**: new feature, backward compatible (e.g. new config field
  with sensible default)
- **Patch**: bug fix only

## Reporting bugs

Open an issue with:

1. **Device** — model, Android version, vendor build (e.g. Infinix
   X695C, Android 11, RP1A.200720.011).
2. **rsc version** — `adb shell /vendor/bin/rsc --version` (or
   check the startup log line `rsc v0.X.X starting`).
3. **Symptom** — what you expected vs what happened.
4. **Log excerpt** — relevant lines from `/data/adb/rsc/rsc.log`
   (set `debug = true` first if the issue is not obvious).
5. **SELinux denials** — `adb logcat -d | grep avc.*rsc`.
6. **Stats dump** — last `stats: ticks=...` line from the log.

Use the bug report template (`.github/ISSUE_TEMPLATE/bug_report.md`).

## Device compatibility reports

If you test rsc on a device not in the compatibility list, please
open an issue with:

- Device model + SoC + Android version + vendor build
- Whether `/proc/mtk_battery_cmd/` and `/sys/devices/platform/battery/`
  paths exist on your device
- Whether the stock SELinux policy allows the netlink socket (check
  `logcat | grep "uevent listener init failed"`)
- Whether the daemon works correctly (charging cut-off fires, thermal
  delimiter toggles, etc.)
- Any modifications you had to make (different sysfs paths, different
  CIL types, etc.)

We'll add your device to the compatibility table in README.md.

## License

By contributing, you agree that your contributions will be licensed
under the MIT License.
