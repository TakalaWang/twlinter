# Discord bot

This adapter listens to registered Discord channels and delegates terminology
detection, Tier 1/Tier 2 rules, and post-fix validation to
`twlinter::core::CoreEngine`. When a message needs a correction, Gemini is used
automatically for context-aware rewriting; deterministic rules remain the
fallback when Gemini is unavailable or its output fails validation.

## Run

1. Create a Discord application and bot, enable the Message Content Intent,
   and invite it with the `bot` and `applications.commands` scopes plus
   permission to read/send messages.
2. Copy `config.example.env`, fill in the tokens, and export the variables.
3. Start the bot from the repository root:

```bash
cargo run --release --features discord --bin twlinter-discord
```

`GEMINI_API_KEY` may be omitted. Without it, deterministic corrections still
work and unresolved context remains unchanged. The channel file defaults to
`twlinter-channels.json` and can be overridden with `TWLINTER_CHANNELS_FILE`.

After the bot connects, a server administrator uses these commands in the
channel where the bot should operate:

- `/twlinter enable` — register the current channel.
- `/twlinter disable` — stop replies in the current channel.
- `/twlinter status` — list registered channels.

Users then write ordinary messages. No rewrite command is needed; TWLinter
replies only when the registered channel's message needs a zh-TW correction.
