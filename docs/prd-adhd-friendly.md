# Temino: ADHD-Friendly Terminal PRD

2026-10-28 · John Zheng

## Summary

Temino should guard the user's attention: get them back into a task fast, tell them only when something needs them, and keep everything else quiet. Three P0 features do most of that: **agent attention states**, **tab recap on return**, and **focus mode**.

## Problem

A terminal with many tabs and AI agents running in parallel is hard on ADHD attention in three ways:

- **Lost context.** After switching away, the user returns to a tab and can't tell what they were doing, what last ran, or whether it worked.
- **Missed or noisy signals.** An agent waiting for input looks the same as one still working, so it either sits idle for minutes or the user checks every tab compulsively.
- **Visual noise.** Every pane, tab and scrolled-off build log competes equally for attention, so focusing on one task takes active effort.

## Goals and principles

**Goals**

- Cut the time to resume a stale tab to one glance (under 3 seconds).
- No agent waits on the user unnoticed for more than a minute while Temino is running.
- Let the user narrow the screen to one task with a single keystroke.

**Non-goals**

- Task management, timers or Pomodoro features. Temino stays a terminal.
- Gamification, streaks or productivity scoring.
- Changing shell behaviour. Everything works from signals Temino already sees (output, OSC 133 prompt marks, cwd, agent status).

**Principles**

1. **Calm by default.** Motion and color are reserved for things that need the user. Nothing blinks unless asked.
2. **Interrupt by severity.** *Needs input* may interrupt; *done* is visible but silent; *working* is ambient.
3. **Every feature is optional** and has a setting in the ⌘, Settings tab.
4. **Glanceable over readable.** Prefer a color, icon or one dim line to a panel the user has to read.

## P0: Agent attention states

Each tab running an agent shows one of three states in the sidebar, and only *needs input* is allowed to interrupt. This builds on the agent status and colored sidebar tabs from PR #12.

**User story.** I start three Claude Code sessions and switch to a browser. When one asks a question, I find out within seconds; I am not pinged when the others are just working.

| State | Sidebar look | Sound | macOS notification | Dock |
| --- | --- | --- | --- | --- |
| Working | Dim spinner, tab tint unchanged | None | None | None |
| Needs input | Slow pulse (about 1.5 s cycle) in an accent color | Optional soft chime | Yes, if the window isn't focused | Badge count |
| Done | Check mark, holds until the tab is viewed | None | Optional, off by default | None |

**Requirements**

- Detect *needs input* from the agent's prompt or status signal. Fall back to "no output for N seconds while the agent is mid-turn" (default 10 s).
- Clicking the notification focuses Temino and selects that tab and pane.
- The pulse stops as soon as the tab is viewed. *Done* clears on view.
- With reduce motion on (from macOS or Temino settings), the pulse becomes a solid accent dot.
- Keybinding to jump to the next tab that needs input (proposed ⌘J).

**Out of scope:** per-agent rules, and notifications on Haiku in v1.

## P0: Tab recap on return

When the user returns to a pane that has been idle for a while, a single dim line answers "where was I?" without them having to scroll.

**User story.** I come back from a meeting to a tab I left 40 minutes ago. Without scrolling I can see the last command, whether it passed, the branch, and how long ago it ran.

**Example**

```
↩ idle 40m · last: cargo test ✗ exit 101 · 2 failed · ~/Developer/temino (emoji-graphemes)
```

**Requirements**

- Shown when a pane is focused after at least N minutes idle (default 10).
- Content: idle time, last command and exit status (from OSC 133 marks), cwd, git branch if any, running agent name if any.
- Drawn as an overlay above the prompt, not written into scrollback, so it never pollutes copy/paste.
- Fades after 6 s or on the first keypress. A key re-shows it (proposed ⌘I).
- If shell integration is missing, show only the idle time and cwd.

**Out of scope:** AI summaries of the pane's output in v1.

## P0: Focus mode

One keystroke narrows Temino to the pane the user is working in. Pressing it again brings everything back exactly as it was.

**User story.** I'm debugging one thing and the sidebar and other panes keep catching my eye. I hit ⌘⇧F and only my current pane is bright.

**Requirements**

- Toggle with a keybinding (proposed ⌘⇧F) and a menu item.
- Hides the sidebar and dims the other panes to about 35% opacity. A setting can hide them instead of dimming.
- *Needs input* still breaks through, as a small pill in the top corner ("agent in tab 3 needs you"). *Done* and *working* are suppressed until focus mode ends.
- Leaving focus mode restores the layout, sidebar width and splits exactly.
- The state is saved per window and restored on relaunch.

## Backlog (P1 / P2)

These follow the P0s. Most reuse the same signals: OSC 133 marks, agent status and idle timers.

| Priority | Feature | What it does | Relies on |
| --- | --- | --- | --- |
| P1 | Long-command notify | Notifies when a command that ran 30 s or longer finishes, with its duration and pass/fail | OSC 133 marks |
| P1 | Error gutter + jump | Red mark beside failed commands; a key jumps to the last error | OSC 133 exit codes |
| P1 | Auto-named tabs | Names each tab after its agent, git branch or running command instead of "zsh" | cwd tracking, agent status |
| P1 | Calm options | Cursor that doesn't blink, reduced motion, muted palette preset | Settings tab |
| P2 | Per-tab sticky note | One line in the sidebar saying why the tab exists | Session restore |
| P2 | Collapse long output | Folds each command's output to one line, which expands on click | OSC 133 marks |
| P2 | Idle tab fade | Tabs untouched for hours fade in the sidebar; offer to close them | Idle timers |
| P2 | Quick capture | A hotkey appends a stray thought to an inbox file without leaving the pane | None |

## Success metrics, rollout, open questions

**Success metrics** (self-reported, since Temino has no telemetry)

- Agents rarely sit in *needs input* unnoticed for over a minute.
- Fewer manual tab sweeps to check on agents.
- Focus mode gets used daily, not just tried once.

**Rollout**

1. Agent attention states: one PR, macOS first, then Haiku without notifications.
2. Tab recap: one PR; needs an idle timer and last-command record per pane.
3. Focus mode: one PR; reuses layout save/restore.
4. P1 backlog items as small separate PRs.

**Open questions**

- How do we reliably detect *needs input* for Claude Code versus other agents? Is there a status signal, or only the output-silence heuristic?
- Do the proposed keybindings (⌘J, ⌘I, ⌘⇧F) clash with apps people run inside Temino?
- Should focus mode also mute notifications from other apps (macOS Focus), or stay inside Temino?
