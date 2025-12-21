---
title: Doctor probe capability + skip semantics
stage: 1
feature: Doctor
---


# RFC 0002: Doctor probe capability + skip semantics

# Intent
Define a capability model for `asahi-setup doctor` probes and a consistent, truthful “skip” mechanism so that:
- reports remain high-signal
- missing privileges do not masquerade as “no problems”
- users are guided toward the primary success path: `sudo asahi-setup doctor`

# Problem
`asahi-setup doctor` is valuable only if it is:
- **truthful**: no “empty success” caused by missing permissions
- **consistent**: a stable-enough report shape across runs and machines
- **actionable**: clear next steps instead of ambiguous failures

In practice, probes fall into multiple privilege categories:
- some are unprivileged and always safe
- some can be made privileged via *subprocess sudo* (because we spawn a command)
- some require the *current process* to have privileges (e.g. native journald via libsystemd)

If we treat these categories the same, we get misleading output:
- journald reads returning 0 entries can be interpreted as “no logs” when it’s really “no access”
- `--sudo`/`--no-sudo` can create confusion if a probe cannot actually be elevated

# Goals
- Define probe categories and the expected behavior in each category.
- Make the skip mechanism explicit and reportable (human + JSON).
- Encourage the “happy path” of running the full doctor under sudo.
- Preserve backward compatibility for saved reports.

# Non-Goals
- Designing a generic, reusable “probe framework” outside this project’s needs.
- Freezing probe keys or report contents permanently (we want stability, but not at the cost of freezing the implementation).

# Model

## Capability classes
Each probe belongs to one of these classes:

1) **Unprivileged**
- Expected to run without root.
- Failure should be treated as “probe failed” (captured in `commands`/stderr), not as a skip.

2) **Sudo-subprocess**
- Implemented by spawning an external command.
- Behavior:
  - try unprivileged first (avoid prompting)
  - if the failure looks like a permission problem, retry with sudo (when allowed)
  - if sudo is disallowed/unavailable, record the probe as skipped

3) **Process-privileged**
- Requires the current `asahi-setup` process to have permissions.
- Example: native journald access via libsystemd.
- Behavior:
  - if capability is not present, omit the probe and record a skip
  - do not attempt to “sudo retry” by spawning, because elevation does not apply

## Skip semantics
When a probe is skipped:
- The probe’s output should not appear in `commands`/`files` as an empty placeholder.
- The probe should be recorded in a top-level `skipped` map in the report JSON.
- The human output should include an aggregated “Skipped probes …” section.

Skip reasons should:
- state what capability is missing (e.g. “requires sudo to read system journal”)
- point to the primary remediation (`sudo asahi-setup doctor`)
- optionally clarify when `--sudo` cannot help (process-privileged probes)

## Key decision: prefer skip over partial output
When a probe cannot run due to missing capability, we prefer:
- omit + record a skip

…over:
- emitting empty output that could be mistaken for “everything is fine”

# Data model contract
- `DoctorReport` includes `skipped: BTreeMap<String, String>`.
- The field must be `#[serde(default)]` so older reports can be parsed.
- `doctor-diff` must include `skipped` changes.

# Current implementation status
As of Dec 2025, this is implemented in `asahi-setup`:
- report schema includes `skipped`
- human output includes an aggregated skipped-probes section
- `doctor-diff` includes skipped changes
- native journald probe is capability-gated (process privileges)
- `journalctl` probe uses “try unprivileged then sudo fallback” (subprocess privileges)

# Open questions
- Should skip reasons be structured (e.g. `{ capability, remediation }`) instead of strings?
- Should we surface “capabilities detected” as a separate top-level field (e.g. `capabilities: { can_read_system_journal: bool }`)?
- How stable do we want probe keys to be (human readability vs long-term diffability)?

# Verification
- `--no-sudo` runs should:
  - avoid sudo prompts
  - record all privilege-blocked probes in `skipped`
- `sudo asahi-setup doctor` should:
  - produce maximal coverage
  - avoid recording skips for probes that are now runnable
- `doctor-diff` should report additions/removals/changes for skipped probes.
