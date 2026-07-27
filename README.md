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
source ~/.bashrc
```

Open the manager:

```bash
easy-ssh
```

The installer adds a marked shell function to `~/.bashrc`, adds
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
easy-ssh remove demo
easy-ssh doctor
```

Projects are stored in
`${XDG_CONFIG_HOME:-~/.config}/easy-ssh/projects.toml`. Passwords are never
stored; OpenSSH continues to handle keys, agents, and password prompts.
