# easy-ssh

`easy-ssh` is a terminal project manager for SSH connections. Save a project
name, its connection details, and a remote working directory, then connect with:

```bash
ssh demo
```

Only the exact interactive form `ssh <project>` is intercepted. Commands such
as `ssh -v server`, `ssh server uptime`, `scp`, and `rsync` continue to use
OpenSSH normally.

## Install

Build and install the binary:

```bash
cargo install --path .
easy-ssh install
```

`easy-ssh install` detects Bash or Zsh from `$SHELL`. Reload the startup file it
reports after installation:

```bash
# Bash
source ~/.bashrc

# Zsh
source ~/.zshrc
```

Select a shell explicitly when detection is unavailable, or configure both
shells by repeating the option:

```bash
easy-ssh install --shell zsh
easy-ssh install --shell bash --shell zsh
```

Open the manager:

```bash
easy-ssh
```

The installer adds a marked shell function to the selected startup file, adds
`Include ~/.ssh/easy-ssh.conf` to `~/.ssh/config`, and creates backups before
changing existing files.

## CLI

Create a new SSH connection:

```bash
easy-ssh add demo \
  --host server.example.com \
  --user deploy \
  --port 2222 \
  --identity ~/.ssh/id_ed25519 \
  --path ~/projects/demo
```

Use a connection already defined in `~/.ssh/config`:

```bash
easy-ssh add demo --ssh-alias production --path ~/projects/demo
```

Other commands:

```bash
easy-ssh list
easy-ssh edit demo --path /srv/demo
easy-ssh connect demo
easy-ssh connect demo --terminal ghostty
easy-ssh session new demo feature-review --agent claude
easy-ssh session list demo
easy-ssh session open demo feature-review
easy-ssh session open demo --all
easy-ssh connect demo --agent codex
easy-ssh remove demo
easy-ssh doctor
easy-ssh doctor --shell bash --shell zsh
```

Projects are stored in
`${XDG_CONFIG_HOME:-~/.config}/easy-ssh/projects.toml`. Passwords are never
stored; OpenSSH continues to handle keys, agents, and password prompts.

## Normal connections and agent sessions

Press `s` in the TUI to choose Ghostty as the default terminal, or use the
`--terminal ghostty` override above. Ghostty mode uses the same plain SSH and
remote login-shell flow as a manual Ghostty connection.

`easy-ssh connect demo` always opens the normal project shell. It never uses
tmux and closing Ghostty ends that connection exactly as it would after a manual
SSH command.

Persistent Claude and Codex work uses named remote tmux windows. Create one
with `easy-ssh session new demo <name> --agent claude|codex`. Closing Ghostty
detaches it; opening the session later returns to the same live agent window.
When a saved session is no longer live, Easy SSH starts its saved agent
conversation again (Claude resumes its stored ID; Codex opens its resume picker).
tmux must be installed on the remote host for agent sessions only.

In the Projects TUI, Enter opens the normal connection and Tab opens the
selected project's session picker. There, Enter opens one session, `a` attaches
the whole workspace, `n` creates a named Claude/Codex session, and `c` cycles
the session's saved color.

When adding or editing a project, Remote folder offers fuzzy directory matches
at the typed path's current level. Up/Down selects a match, Tab inserts it, and
Esc dismisses the list. Every text input supports Left/Right, Home/End,
Backspace, and Delete.

## Connection lifetime

For connections created with `--host`, easy-ssh configures OpenSSH to send a
server-alive probe every 30 seconds and disconnect after three unanswered
probes. This keeps healthy idle connections active and detects broken network
links in about 90 seconds. Projects that use `--ssh-alias` keep the settings
from the user's SSH config unchanged.

Keepalive does not reconnect a dropped normal shell or preserve arbitrary
programs. Use named agent sessions when work must survive closing Ghostty.
