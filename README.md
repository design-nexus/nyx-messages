# Nyx Messages

SMS and RCS for the Nyx Suite.

Pairs with Google Messages on an Android phone via `nyx-messagesd` (Gaia sign-in + emoji confirm). The GTK app is MIT; the daemon is AGPL (libgm).

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nyx-messages/main/install.sh | bash
```

Building needs Rust and Go. To remove it, run the same line with `uninstall.sh` in place of `install.sh` (add `bash -s -- --purge` to also remove its settings, data and phone pairing).

For UI-only checks without a phone, set `NYX_MESSAGES_MOCK=1` (sample chats).
