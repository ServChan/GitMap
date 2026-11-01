# User Guide

The interface is in Russian, so the labels below are given in Russian with their meaning.

## Folder Overview

The first screen lists everything worth acting on:

- **Не отправлено** (not pushed): commits that exist only on this computer.
- **Не закоммичено** (not committed): work sitting in files, not yet in history.
- **Только на этом диске** (only on this disk): the project has no remote.
- **Заброшенные ветки** (abandoned branches): not merged and unchanged for over three months.
- **За неделю** (this week): where work happened.
- **Родственные проекты** (related projects): projects that share a root commit, such as builds for another version.

Bare repositories count as backups: they appear in the list, but not in the overview or among related projects.

Choose a folder with the button or drag it into the window.

## Open Project

The map is live: new commits slide onto their line, and uncommitted work shows as a dashed stub from HEAD. Panel buttons open the project in VS Code, a terminal, Explorer or on its website.

Click a file in a commit to see the changed lines. The «Сравнить» (compare) button and a click on a second commit compare any two commits, and «Отличия от main» (differences from main) shows only what was done in the branch.

«Сохранить PNG» (save PNG) captures the current view at double resolution, and «Сохранить SVG» (save SVG) exports the whole map as vector graphics.

## Reading the Map

| On the map | Meaning |
|---|---|
| thick line through the centre | the main branch: `origin/HEAD`, otherwise `main`, `master`, `trunk`, `develop`, `dev`, otherwise whatever is checked out |
| dashed line through the centre | the main branch has ended while others continue |
| dashed stub from HEAD | uncommitted work |
| thick coloured line with a sign | a branch in progress: its pointer is not merged into main |
| thin line | a merged branch. If its pointer was deleted, the name comes from the merge message |
| tick | an ordinary commit |
| ring | an event: a merge, a fork or a pointer |
| crossbar | the end of a line |
| label at 45° | a tag, or a pointer that differs from the line name |
| ring with a signal and «вы здесь» (you are here) | `HEAD`, what is checked out in the working folder |
| train | the route is alive: trains run on branches in progress |
| strip at the bottom | how much work went on at each moment of history; drag it to move around |

When animations are turned off in Windows, the app runs without them.

## Controls

| Action | How |
|---|---|
| move the map | drag with momentum, arrow keys, the bottom strip |
| zoom | mouse wheel, `+`, `-` |
| whole map, latest events | `Home`, `End` |
| select a commit or branch | click on the map or on a row on the right |
| clear the selection | `Esc` or a click on empty space |
| find a commit | `Ctrl+F`: text, author, hash prefix |
| reload the repository | `F5` |
| move between controls | `Tab`, `Enter` |

## Limits

History is read for up to 60,000 commits, newest first. When it is longer, the map title says so.

Screen readers are not supported yet.
