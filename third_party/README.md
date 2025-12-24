# Third-party upstreams

This project sometimes needs small, local changes to upstream projects (e.g. `muvm`, FEX, etc.) while we decide whether to submit them upstream.

## Policy

- **Do not commit full upstream checkouts** under `third_party/`.
  - `third_party/**` is ignored by default.
- **Do commit our local changes as a patch series** under `third_party/patches/<project>/`.
  - These patches are the record of what we changed and why.

This keeps the main repo reviewable and makes it easy to refresh our work onto newer upstream versions.

## Workflow

### 1) Clone upstream locally (not tracked)

Example for `muvm`:

- `git clone https://github.com/AsahiLinux/muvm.git third_party/muvm`
- `cd third_party/muvm`
- `git remote add upstream https://github.com/AsahiLinux/muvm.git`
- (optional) `git remote add fork git@github.com:<you>/muvm.git`

### 2) Create a topic branch for our changes

- `git checkout -b asahi-env/<topic>`
- Make changes; commit them normally.

### 3) Export patches into this repo

From inside the upstream checkout:

- `git format-patch --output-directory ../../third_party/patches/muvm/<topic> upstream/main..HEAD`

(Adjust `upstream/main` to the upstream branch you’re tracking.)

### 4) Refresh onto a newer upstream

- `git fetch upstream`
- `git rebase upstream/main` (resolve conflicts)
- Re-export the patch series (overwrite the patch directory)

### 5) Submitting upstream PRs

- Push your topic branch to your fork:
  - `git push fork asahi-env/<topic>`
- Open a PR against upstream.
- Once merged upstream, delete the corresponding patch series from `third_party/patches/`.

## Notes

- Patch directories should be small, focused, and named by topic.
- If a change becomes long-lived, consider creating an RFC before it grows further.
