# Troubleshooting

## keyd safety

If you render your keyboard unusable with a bad keyd config, keyd supports a panic sequence:

- `<backspace>+<escape>+<enter>`

## Privileged probes

Some probes need sudo (e.g. `libinput list-devices`).

The intended pattern is:

- ask for sudo once (`sudo -v`)
- run a bounded set of read-only commands
