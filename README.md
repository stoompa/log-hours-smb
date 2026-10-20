# automate-smb

Terminal app for logging working hours in SMB HR from a plain-English phrase.

```
automate-smb setup             # company tag, 1Password item, API key reference
automate-smb                   # week-calendar TUI (press ? for keys)
automate-smb parse <phrase>    # show how Claude reads a phrase (no SMB access)
automate-smb week [YYYY-MM-DD] # print entries already logged that week (read-only)
```

In the TUI, move with ←/→ (days) and [ ] (weeks); weeks load from SMB as you go.
`a` opens a time picker, `n` stages your normal day, `w` fills empty weekdays,
`/` lets Claude turn a phrase into blocks, and `s` reviews and submits new blocks.

Add `--headed` to watch the browser. Needs Google Chrome, the 1Password CLI (`op`)
with desktop-app integration, and an Anthropic API key (`ANTHROPIC_API_KEY` or an
`op://` reference in `~/.config/automate-smb/config.toml`).

Login and timesheet details: [docs/smb-notes.md](docs/smb-notes.md).
