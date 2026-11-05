---
type: Finding
title: Memory card saves
description: How sly-rs gives Sly 1 a memory card - the card manager's file system over a PCSX2-compatible image, the card server's calls as Sly 1 makes them, how play, replay and run hold cards so replays stay exact - and the cross-check with PCSX2, which reads sly-rs's saves and writes the same card layout byte for byte.
tags: [finding, memory-card, iop, mcserv, mcman, saves, determinism, phase-7]
generated: { by: claude-code/claude-opus-5-5, at: 2026-11-05T04:07:34Z }
sources:
  - id: ps2sdk
    resource: https://github.com/ps2dev/ps2sdk (iop/memorycard/mcman, iop/memorycard/mcserv, ee/rpc/memorycard, common/include/libmc-common.h), read from a shallow clone for facts, 2026-11-04
    title: ps2sdk's reimplementation of libmc, mcserv and mcman
  - id: calls
    resource: sly-rs replay with card = "formatted" and SLY_RS_LOG=iop::mc=debug, Sly 1 from power-on, answering Yes to "create a Sly Cooper Save File", 2026-11-04
    title: The card calls Sly 1 makes
  - id: pcsx2
    resource: tools/pcsx2-frames.sh with CARD and CARD_OUT, PCSX2's software renderer, Sly 1 from power-on with a card written by sly-rs, and with a blank card answered Yes, 2026-11-04
    title: Cards exchanged with PCSX2
  - id: play
    resource: sly-rs play in gamescope --backend headless with timed key presses, recorded and replayed, 2026-11-04
    title: Saving from play, replayed
  - id: user
    resource: human:august, 2026-11-04
    title: The user's go-ahead for the plan's next step (memory card saves)
---

# Memory card saves

The plan's next step after the speed work was phase 7's memory card
saves.[^user] Until now the IOP's card server answered "no card", and Sly 1
showed its "no memory card" dialog at every boot.

## What Sly 1 does with a card

Logged with a blank card in slot 1:[^calls]

| When | Calls |
| --- | --- |
| Boot | `GetInfo` (the first answer is -1, a new card; then 0), then `GetDir "/BASCUS-97198YAOTWTD!"` with no table and a maximum of -1, to count its save directory |
| No save directory | the dialog "No Sly Cooper Save File ... Do you want to create a Sly Cooper Save File?" (Yes/No, No selected) |
| Yes | `GetEntSpace "/"`; `mkdir` of the directory; files written and closed one by one: a 10-byte file named like the directory, `icon.sys` (964 bytes), `sly.ico` (21,656), then `save1`, `save2`, `save3` (6,656 each), each checked by a `GetDir` of its name; about 49 KB in all |
| Save directory present | each `saveN` listed, opened and read whole |
| Afterwards | `GetInfo` about every 20 fields, to notice the card being taken out |

The counting `GetDir` is the one call ps2sdk's `mcserv` would answer
differently: its loop sends no entry for a maximum below 1, which would
leave Sly 1 never finding its saves. Sony's server must count every
match there, and sly-rs does.[^ps2sdk]

## How it is built

- **`ps2-memcard::Files`**: the card's file system as Sony's card
  manager (`mcman`) keeps it, from ps2sdk's reimplementation:[^ps2sdk] paths
  from the root or a current directory, three handles, files created with
  mode 0x8417 and no cluster until written, directories with mode 0x8427
  and `.`/`..` entries pointing back at their parent's entry, deleted
  entries keeping their place and their chain (bit 31 cleared) until
  reused, clusters taken lowest first, a written file marked closed (0x80)
  when closed, `?` and `*` in listings. Results are `mcman`'s codes. Every
  page written gets its ECC, and the spare's last four bytes are zeros, as
  `mcman` writes them.
- **`ps2-iop`'s `mcserv`**: the XMCSERV call numbers (`GetInfo` 0x01 with
  its 192-byte reply record, open 0x02 to `GetEntSpace` 0x12), data moved
  straight to and from EE memory: a read fills the whole buffer and tells
  `libmc` nothing is left to copy; a write takes its first bytes from the
  call and the rest from EE memory. Port 0, slot 0 holds the card; the
  other slots are empty.
- **Time stamps** come from the same fixed clock `sceCdReadClock` answers
  (2002-08-24 12:00), so the same session writes the same card.

## Cards and replays

A card changes what the game does at boot, and every existing recording
was made without one, so a card is only ever inserted when asked for:

| | The card |
| --- | --- |
| `play` | the image under `[cards]` in `sly-rs.toml` (default `cards/GAME.ps2` beside it), made formatted when missing, written again (to a file beside it, then renamed) on a thread of its own each time the game completes a change; `--card PATH` or `--no-card` instead |
| `play --replay` | the replay's card, in memory only |
| `play --record` | the recording keeps the card the session started from: `card = "formatted"`, or a copy of the image beside it (`NAME.card.ps2`) |
| `replay` | the replay's `card` (`formatted` or an image's path), none if absent; read, never written; `--card-out` writes the card as the game left it |
| `run` | none unless `--card`; `--card-out` likewise |

Card images hold the user's saves: `.gitignore` keeps `*.ps2` and `cards/`
out of the repository. The new-game replay and the user's earlier
recordings run without a card and keep their hashes.

## Checks

- **Unit tests**: files written, listed, read back, replaced, deleted and
  undeleted; directories growing a cluster for their third entry; handle
  limits and renames; the server's reply layouts.
- **PCSX2 reads sly-rs's card**:[^pcsx2] booted with the card sly-rs wrote
  when answering Yes, PCSX2 goes from the logo movie straight to the intro,
  with no dialog: its card manager found the save.
- **sly-rs reads PCSX2's card**:[^pcsx2] PCSX2, given a blank card and the same
  answer, wrote a card that sly-rs boots from, reading the three slots and
  leaving the image unchanged.
- **The same layout**: the two cards have the same entries (modes, sizes,
  clusters, chains, the directory growth clusters) and the six files are
  byte for byte the same. What differs: time stamps (PCSX2 uses the real
  clock), the card manager's backup blocks (its write journal, which
  sly-rs does not keep; PCSX2 reads cards without it), and, before the fix
  above, the spare's last four bytes. Loading accepts erased pages (all
  0xFF, spare included) without an ECC, as the backup blocks are.
- **Play saves, replays reproduce**:[^play] `play` in a headless gamescope,
  keys sent on a timer, created the save on a new card file and recorded
  the session with `card = "formatted"`; replaying it gave the same state
  hash and a card byte for byte the same as the file `play` wrote. A second
  session from that card recorded a copy of it and replayed to its hash too.

## Not done

- A save of progress during play (the "Save Game" option, auto-save) and a
  load from the title have not been driven yet; they use the calls above.
- Sony's `mcman` keeps a journal in the backup blocks to survive a write
  cut short; sly-rs writes the image whole, renamed into place.
- `play` writes the image when the game completes a change (closes a
  written file, creates, deletes or renames one): a file still being
  written when the window closes is lost, as on a console switched off
  mid-save.
- `Unformat`, page-level reads and writes, and PS1 cards are not answered.

[^ps2sdk]: ps2sdk's reimplementation of libmc, mcserv and mcman
[^calls]: The card calls Sly 1 makes
[^pcsx2]: Cards exchanged with PCSX2
[^play]: Saving from play, replayed
[^user]: The user's go-ahead for the plan's next step (memory card saves)
