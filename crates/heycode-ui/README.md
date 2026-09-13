# heycode-ui

`heycode-ui` owns UI-neutral contribution contracts. Plugin `ui` publishes the
effect-owned `UiRegistry` for typed panel/dialog/status handles and the
`SettingsUiRegistry` under service `settings-ui`.

Settings surfaces are derived from the authoritative `heycode-settings` schema by
default. A plugin may claim its namespace for a custom surface as a Context
effect; shutdown removes only that token and restores schema derivation. Secret
controls contain only a configured flag, managed and layered origins remain
explicit, and schema shapes the generic UI cannot edit become visible
unrenderable rows rather than disappearing. This crate decides what a safe UI
may show; terminal interaction and revision-CAS writes remain owned by
`heycode-tui`.

The UI-neutral keymap has thirteen closed, conflict-checked actions. A05/U18 adds
persisted action `queue-follow-up` with shipped chord Tab; it remains distinct
from Enter/`submit` and Esc/`interrupt`, may be rebound through the same
Settings CAS path, and cannot collide with an untouched default. Whether Tab
queues or remains composer input is live interaction state owned by the TUI,
not encoded into this registry.

U22 adds persisted `cycle-side-panel` with shipped chord Ctrl+B. The owning TUI
registers Diff/Jobs/Agents as ordinary `UiSlot::SidePanel` contributions, so
collisions, attribution and token-checked teardown follow the same registry
contract as every other panel.

CMD10 adds a schema-backed `ui-preferences` value containing a validated theme
contribution id, Vim mode, the closed `full|compact` header density and two
independent footer toggles. `SettingsBackedUiPreferences` loads the exact
revision and requires that revision for replacement, so a stale editor cannot
overwrite a newer generation. The theme and Vim convenience writes preserve
the shell choices rather than rebuilding defaults. Theme existence remains a
live `UiRegistry` decision, allowing effect-owned contributed themes without
freezing their ids into the settings schema.

`SettingsBackedKeymap` likewise exposes a versioned load and `store_at`; the
ordinary store now reads the authoritative revision before committing. This
keeps `/keymap` on the same transactional boundary as `/settings` while still
persisting only overrides rather than today's complete defaults.

Focused verification:

```sh
cargo clippy -p heycode-ui --all-targets -- -D warnings
cargo test -p heycode-ui
```
