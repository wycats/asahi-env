# Safe Changes (check/apply/rollback)

## Goal

Make changes that are fast to test and easy to undo.

## Pattern

- Prefer editing a single file per operation.
- Make edits **idempotent**.
- Before writing, validate the candidate state if the system provides a validator.
  - Example: `keyd check` before updating `/etc/keyd/default.conf`.

## Rollback

- Keep changes small enough that rollback is either:
  - restoring one file from a backup, or
  - applying the inverse of the operation.
