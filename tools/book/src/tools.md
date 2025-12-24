# Tools

This section describes each tool’s surface area (commands, targets, and safety properties).

## Conventions

- `check`: reads state and reports drift; does not modify the system.
- `apply`: makes changes (supports `--dry-run` when possible).
- `verify`: proves the change took effect (often with a targeted probe).
