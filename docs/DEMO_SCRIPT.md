# Demo Recording Script

> Used to record the GIF/asciinema for the r/osdev "Show & Tell" post.
> Goal: 60-90 seconds of footage that shows CLUU is real and reasonably
> distinctive without overstaying its welcome.

## Setup

1. Start with a fresh `cargo xtask build`.
2. Pre-resize your terminal to a sane size for embed (recommended: 100x32).
3. Tool of choice:
   - **asciinema** (preferred — small, scrollable, terminal-true):
     ```
     asciinema rec /tmp/cluu-demo.cast --idle-time-limit 1.5 --command 'cargo xtask run'
     ```
   - **GIF** via `agg` (asciinema → GIF) after recording:
     ```
     agg /tmp/cluu-demo.cast /tmp/cluu-demo.gif --speed 1.5
     ```

## The script (talk-through, ~75 seconds)

Type slowly enough to read. Pause ~1 sec between commands so the GIF
is followable when slowed down.

```
# Wait for boot to settle, login prompt appears.
admin
admin

# (You're in. Show motd renders.)

cat /etc/welcome.txt
# Pause ~3 seconds — let viewers read.

ls /
# (Top-level dirs visible.)

ls /var/images
# (Show the container library — there are ~25 of them.)

cat /etc/architecture.txt
# Pause ~3 seconds — distinctive content.

# Mount-policy demo:
spawn mkdir /tmp/demo
spawn mkdir /tmp/demo/inner
spawn rm -r /tmp/demo
# All three succeed because /tmp inherits across spawns.

# History recall:
# Press ↑ — see "spawn rm -r /tmp/demo" come back.
# Press ↑ again — "spawn mkdir /tmp/demo/inner".
# Press Enter on something. Or Ctrl-C to clear.

# Process visibility:
top
# Wait 2 seconds — show the live process list.
# Press 'q' to quit.

# A taste of MicroPython:
spawn micropython -c "print('hello from micropython on cluu')"

# End on a clean prompt.
```

## What NOT to show

- Do not pipe (`cat | grep`) — it doesn't work.
- Do not redirect (`> file`) — it doesn't work.
- Do not invoke `vi` / `nano` / `less` — they aren't there.
- Do not show recording sessions across reboots — keep it one continuous shell.

## Embedding in the post

- Asciinema cast: upload to asciinema.org, embed via player iframe in the post link section.
- GIF: under 5 MB; r/osdev allows imgur/i.redd.it links.

## Re-record if

- Boot log is excessively noisy and obscures the login prompt.
- Any command throws a panic / fault that isn't recoverable.
- The GIF runs over ~90 seconds — viewers won't watch.
